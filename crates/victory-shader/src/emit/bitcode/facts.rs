//! What is known of a float value while the module is built: the range its numbers lie in,
//! whether it may be NaN, and which zeros it may be. The lowering asks before it lowers a
//! console instruction whose exact form costs more where an operand may be zero, infinite or
//! NaN (the legacy multiply, `lower::legacy`).
//!
//! Every rule is the host's arithmetic on what DXIL computes, over-approximated: a value's
//! facts hold for every input of the program. What is not known (a load, a join the lowering
//! fills later, a chain longer than [`DEPTH`]) knows nothing ([`Facts::ANY`]).
//!
//! Zeros: a denormal counts as a zero of its sign (`pz`, `nz`), as a host that flushes reads
//! it; `least` bounds the magnitude of every other number from below. Bounds are widened by
//! [`SLACK`] after each rounded operation, and a bound past the largest float is an infinity.

use super::{Bin, Cast, Module, Op, TypeDef, V};
use crate::emit::lower::legacy::Form;

/// The smallest normal f32.
const NORMAL: f64 = 1.175_494_350_822_287_5e-38;
/// The largest finite f32.
const LARGEST: f64 = 3.402_823_466_385_288_6e38;
/// Relative widening of a rounded bound (f32 rounds by at most 2^-24).
const SLACK: f64 = 1.0 / 1_048_576.0;
/// How deep a value's makers are followed while the module is built.
const DEPTH: u32 = 24;
/// Rounds of [`Module::settled`] before bounds that still grow are widened, and in all.
const WIDEN: u32 = 6;
const ROUNDS: u32 = 24;

/// Bounds of sums and products on the f32s at or beyond the exact ones
/// ([`Facts::outward`]), and `0 - y` as the card computes it: part of `VICTORY_LEGACY_LOOPS`
/// (on), so that with it off every program is as before it, bit for bit.
fn on_floats() -> bool {
    crate::emit::lower::legacy::loops_on()
}

/// No value at all (a join's start in [`Module::settled`]).
const BOTTOM: Facts = Facts {
    lo: f64::INFINITY,
    hi: f64::NEG_INFINITY,
    nan: false,
    pz: false,
    nz: false,
    least: f64::INFINITY,
};

/// What every value of an f32 is known to satisfy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Facts {
    /// Its numbers (zeros as 0) lie in `[lo, hi]`; bounds may be infinite.
    pub lo: f64,
    pub hi: f64,
    /// It may be NaN.
    pub nan: bool,
    /// It may be +0 (or a positive denormal).
    pub pz: bool,
    /// It may be -0 (or a negative denormal).
    pub nz: bool,
    /// Its numbers that are not zeros have at least this magnitude.
    pub least: f64,
}

/// A console operation the lowering made of several host instructions, whose result the
/// facts follow through as the console defines it (the select that fixes zeros hides it).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Made {
    /// `v_max_f32` (`true`) or `v_min_f32` of two values: one of them, -0 below +0, a NaN
    /// operand giving the other.
    MaxMin(bool, V, V),
    /// `v_med3_f32`: one of the three, or the least number among them with a NaN.
    Med3(V, V, V),
    /// The legacy multiply: +0 where an operand is a zero, the product otherwise.
    Legacy(V, V),
}

impl Facts {
    /// Nothing known.
    pub const ANY: Facts = Facts {
        lo: f64::NEG_INFINITY,
        hi: f64::INFINITY,
        nan: true,
        pz: true,
        nz: true,
        least: 0.0,
    };

    /// The facts of the constant with these bits.
    pub fn constant(bits: u32) -> Facts {
        let x = f64::from(f32::from_bits(bits));
        if x.is_nan() {
            return Facts {
                lo: f64::INFINITY,
                hi: f64::NEG_INFINITY,
                nan: true,
                pz: false,
                nz: false,
                least: f64::INFINITY,
            };
        }
        let zero = x.abs() < NORMAL;
        Facts {
            lo: x,
            hi: x,
            nan: false,
            pz: zero && bits >> 31 == 0,
            nz: zero && bits >> 31 != 0,
            least: if zero { f64::INFINITY } else { x.abs() },
        }
    }

    /// Neither infinite nor NaN.
    pub fn finite(&self) -> bool {
        !self.nan && self.lo > f64::NEG_INFINITY && self.hi < f64::INFINITY
    }

    /// Its sign bit is clear wherever it is not NaN (+0 and positive numbers).
    pub fn plus(&self) -> bool {
        self.lo >= 0.0 && !self.nz
    }

    /// Its sign bit is set wherever it is not NaN (-0 and negative numbers).
    pub fn minus(&self) -> bool {
        self.hi <= 0.0 && !self.pz
    }

    /// Never a zero (nor a denormal).
    pub fn nonzero(&self) -> bool {
        !self.pz && !self.nz
    }

    /// A finite number of at least the smallest normal magnitude, of one sign: `Some(true)`
    /// positive, `Some(false)` negative.
    pub fn sign_of_normal(&self) -> Option<bool> {
        if !self.finite() || !self.nonzero() || self.least < NORMAL {
            return None;
        }
        if self.lo > 0.0 {
            Some(true)
        } else if self.hi < 0.0 {
            Some(false)
        } else {
            None
        }
    }

    /// No NaN among the bounds (what an operand without values may leave there).
    fn tidy(self) -> Facts {
        Facts {
            lo: if self.lo.is_nan() {
                f64::NEG_INFINITY
            } else {
                self.lo
            },
            hi: if self.hi.is_nan() {
                f64::INFINITY
            } else {
                self.hi
            },
            least: if self.least.is_nan() { 0.0 } else { self.least },
            ..self
        }
    }

    /// `self` grown to hold `new`, a bound that moves going straight to its end.
    fn widen(self, new: Facts) -> Facts {
        let j = self.join(new);
        Facts {
            lo: if j.lo < self.lo {
                f64::NEG_INFINITY
            } else {
                j.lo
            },
            hi: if j.hi > self.hi { f64::INFINITY } else { j.hi },
            least: if j.least < self.least { 0.0 } else { j.least },
            ..j
        }
    }

    fn join(self, o: Facts) -> Facts {
        Facts {
            lo: self.lo.min(o.lo),
            hi: self.hi.max(o.hi),
            nan: self.nan || o.nan,
            pz: self.pz || o.pz,
            nz: self.nz || o.nz,
            least: self.least.min(o.least),
        }
    }

    fn neg(self) -> Facts {
        Facts {
            lo: -self.hi,
            hi: -self.lo,
            pz: self.nz,
            nz: self.pz,
            ..self
        }
    }

    fn abs(self) -> Facts {
        let (lo, hi) = if self.lo >= 0.0 {
            (self.lo, self.hi)
        } else if self.hi <= 0.0 {
            (-self.hi, -self.lo)
        } else {
            (0.0, self.hi.max(-self.lo))
        };
        Facts {
            lo,
            hi,
            pz: self.pz || self.nz,
            nz: false,
            ..self
        }
    }

    /// May be infinite.
    fn inf(&self) -> bool {
        self.lo == f64::NEG_INFINITY || self.hi == f64::INFINITY
    }

    fn zero(&self) -> bool {
        self.pz || self.nz
    }

    /// A rounded result's bounds: widened, past the largest float an infinity.
    fn rounded(lo: f64, hi: f64) -> (f64, f64) {
        if !on_floats() {
            return Self::widened(lo, hi);
        }
        Self::outward(lo - lo.abs() * SLACK, hi + hi.abs() * SLACK)
    }

    /// The bounds of a sum or product of f32s within `[lo, hi]` before rounding:
    /// [`Facts::outward`] with `VICTORY_LEGACY_LOOPS` (on), [`Facts::widened`] without.
    fn exact_rounded(lo: f64, hi: f64) -> (f64, f64) {
        if on_floats() {
            Self::outward(lo, hi)
        } else {
            Self::widened(lo, hi)
        }
    }

    /// Bounds widened by [`SLACK`], past the largest float an infinity: every rounded bound
    /// without `VICTORY_LEGACY_LOOPS` (as before it, bit for bit).
    fn widened(lo: f64, hi: f64) -> (f64, f64) {
        let widen = |x: f64, up: bool| {
            let w = x + if up { x.abs() } else { -x.abs() } * SLACK;
            if w > LARGEST {
                f64::INFINITY
            } else if w < -LARGEST {
                f64::NEG_INFINITY
            } else {
                w
            }
        };
        (widen(lo, false), widen(hi, true))
    }

    /// The bounds of a result rounded once to nearest from a number within `[lo, hi]`: the
    /// f32s at or beyond them (rounding never passes a float the number does not pass), a
    /// result under the smallest normal possibly flushed to a zero of its sign, one past the
    /// largest float an infinity. (Bounds of products and sums of f32s are exact in f64, or
    /// within 2^-53 of exact, far inside an f32's step.)
    fn outward(lo: f64, hi: f64) -> (f64, f64) {
        let down = |x: f64| {
            if x.is_nan() {
                return f64::NEG_INFINITY;
            }
            let f = x as f32;
            f64::from(if f64::from(f) > x { f.next_down() } else { f })
        };
        let up = |x: f64| {
            if x.is_nan() {
                return f64::INFINITY;
            }
            let f = x as f32;
            f64::from(if f64::from(f) < x { f.next_up() } else { f })
        };
        let (mut lo, mut hi) = (down(lo), up(hi));
        if lo > 0.0 && lo < NORMAL {
            lo = 0.0;
        }
        if hi < 0.0 && hi > -NORMAL {
            hi = 0.0;
        }
        (lo, hi)
    }

    /// Where the bounds keep every number away from zero, no zero, and the bound is a least
    /// magnitude.
    fn clear_zeros(mut self) -> Facts {
        if self.lo >= NORMAL || self.hi <= -NORMAL {
            self.pz = false;
            self.nz = false;
        }
        if self.lo > 0.0 {
            self.least = self.least.max(self.lo);
        } else if self.hi < 0.0 {
            self.least = self.least.max(-self.hi);
        }
        self
    }

    /// The host's product.
    pub fn mul(self, o: Facts) -> Facts {
        let nan = self.nan || o.nan || (self.zero() && o.inf()) || (o.zero() && self.inf());
        let p = |a: f64, b: f64| if a == 0.0 || b == 0.0 { 0.0 } else { a * b };
        let c = [
            p(self.lo, o.lo),
            p(self.lo, o.hi),
            p(self.hi, o.lo),
            p(self.hi, o.hi),
        ];
        let (mut lo, mut hi) = (c[0], c[0]);
        for x in c {
            lo = lo.min(x);
            hi = hi.max(x);
        }
        let (lo, hi) = Self::exact_rounded(lo, hi);
        // (an operand without numbers other than zeros has an infinite least: none known)
        let least = self.least * o.least * (1.0 - SLACK);
        let least = if least.is_nan() { 0.0 } else { least };
        // A zero: from a zero operand, or a product under the smallest normal.
        let zero = self.zero() || o.zero() || least < NORMAL;
        let same = (self.plus() && o.plus()) || (self.minus() && o.minus());
        let opposite = (self.plus() && o.minus()) || (self.minus() && o.plus());
        Facts {
            lo,
            hi,
            nan,
            pz: zero && !opposite,
            nz: zero && !same,
            least,
        }
        .clear_zeros()
    }

    /// A product of a value and itself: never of set sign (NaN aside).
    fn square(self) -> Facts {
        Facts {
            lo: self.lo.max(0.0),
            pz: self.pz || self.nz,
            nz: false,
            ..self
        }
    }

    /// The legacy product: +0 for a zero operand, whatever the other is.
    pub fn legacy(self, o: Facts) -> Facts {
        let zeros = self.zero() || o.zero();
        let p = Facts {
            pz: false,
            nz: false,
            nan: false,
            ..self
        }
        .mul(Facts {
            pz: false,
            nz: false,
            nan: false,
            ..o
        });
        let mut r = Facts {
            nan: self.nan || o.nan,
            ..p
        };
        if zeros {
            r.pz = true;
            r.lo = r.lo.min(0.0);
            r.hi = r.hi.max(0.0);
        }
        r
    }

    /// The host's sum (or an unfused or fused multiply-add's, with `fused`).
    pub fn add(self, o: Facts, fused: bool) -> Facts {
        let nan = self.nan
            || o.nan
            || (self.hi == f64::INFINITY && o.lo == f64::NEG_INFINITY)
            || (self.lo == f64::NEG_INFINITY && o.hi == f64::INFINITY);
        let (lo, hi) = Self::exact_rounded(self.lo + o.lo, self.hi + o.hi);
        let (lo, hi) = (
            if lo.is_nan() { f64::NEG_INFINITY } else { lo },
            if hi.is_nan() { f64::INFINITY } else { hi },
        );
        // Same signs keep the larger magnitude; otherwise only the bounds say.
        let least = if (self.plus() && o.plus()) || (self.minus() && o.minus()) {
            self.least.min(o.least)
        } else {
            0.0
        };
        let zero = lo < NORMAL && hi > -NORMAL;
        Facts {
            lo,
            hi,
            nan,
            pz: zero,
            // -0 + -0, or a negative sum flushed (any zero when fused).
            nz: zero && (fused || lo < 0.0 || (self.nz && o.nz)),
            least,
        }
        .clear_zeros()
    }

    /// The host's quotient, of a finite number of one sign and at least the smallest normal
    /// magnitude (a reciprocal's 1, mostly) by a value of one sign that is never NaN: never
    /// NaN, of the sign of the two, a zero only where it may underflow. Nothing otherwise.
    pub fn div(self, o: Facts) -> Facts {
        let Some(positive) = self.sign_of_normal() else {
            return Facts::ANY;
        };
        if o.nan || !(o.plus() || o.minus()) {
            return Facts::ANY;
        }
        let (x_lo, x_hi) = (
            self.lo.abs().min(self.hi.abs()),
            self.lo.abs().max(self.hi.abs()),
        );
        let (y_lo, y_hi) = (o.lo.abs().min(o.hi.abs()), o.lo.abs().max(o.hi.abs()));
        // A divisor that may be a zero gives an infinity.
        let y_least = if o.zero() { 0.0 } else { y_lo.max(o.least) };
        let small = if y_hi == f64::INFINITY {
            0.0
        } else {
            x_lo / y_hi
        };
        let large = if y_least == 0.0 {
            f64::INFINITY
        } else {
            x_hi / y_least
        };
        let (small, large) = Self::rounded(small, large);
        let small = small.max(0.0);
        let zero = small < NORMAL;
        let up = positive == o.plus();
        Facts {
            lo: if up { small } else { -large },
            hi: if up { large } else { -small },
            nan: false,
            pz: zero && up,
            nz: zero && !up,
            least: small,
        }
    }

    fn max_min(self, o: Facts, max: bool) -> Facts {
        // One of the two (the other where one is NaN).
        let mut r = self.join(o);
        r.nan = self.nan && o.nan;
        if max {
            let mut lo = self.lo.max(o.lo);
            if self.nan {
                lo = lo.min(o.lo);
            }
            if o.nan {
                lo = lo.min(self.lo);
            }
            r.lo = lo;
            // -0 orders below +0: a -0 that meets a value of clear sign loses.
            let beats = |a: &Facts, b: &Facts| a.nz && !(b.plus() && !b.nan);
            r.nz = beats(&self, &o) || beats(&o, &self);
        } else {
            let mut hi = self.hi.min(o.hi);
            if self.nan {
                hi = hi.max(o.hi);
            }
            if o.nan {
                hi = hi.max(self.hi);
            }
            r.hi = hi;
            let beats = |a: &Facts, b: &Facts| a.pz && !(b.minus() && !b.nan);
            r.pz = beats(&self, &o) || beats(&o, &self);
        }
        r.clear_zeros()
    }

    fn med3(f: [Facts; 3]) -> Facts {
        let mut r = f[0].join(f[1]).join(f[2]);
        // Two numbers that are never NaN bound the median (and the least, with a NaN).
        r.nan = [f[0].nan, f[1].nan, f[2].nan]
            .iter()
            .filter(|&&n| n)
            .count()
            >= 2;
        for (i, j) in [(0, 1), (0, 2), (1, 2)] {
            if !f[i].nan && !f[j].nan {
                r.lo = r.lo.max(f[i].lo.min(f[j].lo));
                r.hi = r.hi.min(f[i].hi.max(f[j].hi));
            }
        }
        r.clear_zeros()
    }

    /// DXIL's Saturate: [0, 1], NaN 0.
    fn saturate(self) -> Facts {
        let low = self.lo < 0.0 || self.nan;
        let (mut lo, mut hi) = (self.lo.clamp(0.0, 1.0), self.hi.clamp(0.0, 1.0));
        if self.nan {
            (lo, hi) = (lo.min(0.0), hi.max(0.0));
        }
        Facts {
            lo,
            hi,
            nan: false,
            pz: self.pz || low,
            nz: self.nz || low,
            least: self.least.min(1.0),
        }
        .clear_zeros()
    }
}

impl Module {
    /// Notes that `v` is the console operation `made` (see [`Made`]).
    pub fn made(&mut self, v: V, made: Made) {
        if let V::Inst(i) = v {
            self.made.insert(i, made);
        }
    }

    /// What is known of the f32 `v` (see [`Facts`]) while the module is built: joins not
    /// yet filled know nothing.
    pub fn facts(&self, v: V) -> Facts {
        self.facts_at(v, 0)
    }

    fn facts_at(&self, v: V, depth: u32) -> Facts {
        if let Some(f) = self.constant_facts(v) {
            return f;
        }
        let V::Inst(i) = v else { return Facts::ANY };
        let seen = self.facts_seen.borrow().get(&i).copied();
        if let Some(f) = seen {
            return f;
        }
        if depth >= DEPTH {
            return Facts::ANY;
        }
        // (a join that comes round to itself knows nothing of itself)
        self.facts_seen.borrow_mut().insert(i, Facts::ANY);
        let f = self.transfer(i, &mut |x| self.facts_at(x, depth + 1));
        self.facts_seen.borrow_mut().insert(i, f);
        f
    }

    /// The facts of a value that is no instruction (a constant, or an instruction that
    /// negates or takes the magnitude of one); `None` for any other instruction.
    fn constant_facts(&self, v: V) -> Option<Facts> {
        if let Some(b) = self.float_bits(v) {
            return Some(Facts::constant(b));
        }
        (!matches!(v, V::Inst(_))).then_some(Facts::ANY)
    }

    /// What instruction `i` computes, from what `get` knows of its operands.
    fn transfer(&self, i: u32, get: &mut dyn FnMut(V) -> Facts) -> Facts {
        if let Some(&made) = self.made.get(&i) {
            return match made {
                Made::MaxMin(max, a, b) => get(a).max_min(get(b), max),
                Made::Med3(a, b, c) => Facts::med3([get(a), get(b), get(c)]),
                Made::Legacy(a, b) => {
                    let r = get(a).legacy(get(b));
                    if a == b { r.square() } else { r }
                }
            };
        }
        let float = self.types[self.insts[i as usize].ty.0 as usize] == TypeDef::Float;
        match &self.insts[i as usize].op {
            // An i32 read as a float: the float it was, or a choice among such.
            &Op::Cast(Cast::Bitcast, x) if float || self.is_float_v(x) => get(x),
            &Op::Cast(Cast::UiToFp | Cast::SiToFp, x) if float && !self.narrow(x) => Facts::ANY,
            Op::Cast(Cast::UiToFp, _) if float => Facts {
                lo: 0.0,
                hi: 4_294_967_296.0,
                nan: false,
                pz: true,
                nz: false,
                least: 1.0,
            },
            Op::Cast(Cast::SiToFp, _) if float => Facts {
                lo: -2_147_483_648.0,
                hi: 2_147_483_648.0,
                nan: false,
                pz: true,
                nz: false,
                least: 1.0,
            },
            &Op::Select(_, t, f) => get(t).join(get(f)),
            Op::Phi(incoming) if !incoming.is_empty() => {
                let mut r = BOTTOM;
                for &(x, _) in incoming {
                    r = r.join(get(x));
                }
                r
            }
            &Op::Bin(b, x, y) if float => {
                if b == Bin::Sub && self.float_bits(x) == Some(0x8000_0000) {
                    return get(y).neg();
                }
                let (fx, fy) = (get(x), get(y));
                match b {
                    Bin::Add => fx.add(fy, false),
                    // `0 - y`: the RTX 5090's driver computes it as `-y`, which is -0 for a +0
                    // y where IEEE's difference is +0 (`legacy_short_forms_are_the_consoles` on
                    // the card): either.
                    Bin::Sub if on_floats() && self.float_bits(x) == Some(0) => {
                        fx.add(fy.neg(), false).join(fy.neg())
                    }
                    Bin::Sub => fx.add(fy.neg(), false),
                    Bin::Mul if x == y => fx.mul(fy).square(),
                    Bin::Mul => fx.mul(fy),
                    // (float division: `SDiv` on floats is `fdiv`)
                    Bin::SDiv => fx.div(fy),
                    _ => Facts::ANY,
                }
            }
            Op::Call(f, args) if float => {
                let name = self.funcs[*f as usize].name.as_str();
                let code = args.first().and_then(|&a| self.int_value(a));
                match (name, code, &args[..]) {
                    ("dx.op.unary.f32", Some(op), &[_, x]) => unary(op, get(x)),
                    ("dx.op.binary.f32", Some(op @ (35 | 36)), &[_, x, y]) => {
                        let (fx, fy) = (get(x), get(y));
                        // DXIL's FMax and FMin: either zero for two zeros.
                        let mut r = fx.max_min(fy, op == 35);
                        r.pz = (fx.pz || fy.pz) && r.lo < NORMAL && r.hi > -NORMAL;
                        r.nz = (fx.nz || fy.nz) && r.lo < NORMAL && r.hi > -NORMAL;
                        r
                    }
                    ("dx.op.tertiary.f32", Some(46), &[_, a, b, c]) => {
                        let (fa, fb, fc) = (get(a), get(b), get(c));
                        fa.mul(fb).add(fc, true)
                    }
                    _ => Facts::ANY,
                }
            }
            _ => Facts::ANY,
        }
    }

    /// What is known of every instruction's value once every join is filled: facts that
    /// hold for each instruction given its operands' (each holds at least what its rule gives
    /// from its operands' facts, so by induction over a run every value lies within its
    /// facts), reached from "no value" at every join, bounds that keep moving after [`WIDEN`]
    /// rounds widened to infinity; `None` where that is not reached within [`ROUNDS`].
    fn settled(&self) -> Option<Vec<Facts>> {
        let mut order = Vec::new();
        for b in &self.blocks {
            order.extend(b.phis.iter().chain(&b.body).copied());
        }
        let mut cur = vec![BOTTOM; self.insts.len()];
        for round in 0..ROUNDS {
            let mut changed = false;
            for &i in &order {
                let f = {
                    let table = &cur;
                    let mut get = |x: V| match x {
                        V::Inst(j) => table[j as usize],
                        _ => self.constant_facts(x).unwrap_or(Facts::ANY),
                    };
                    self.transfer(i, &mut get).tidy()
                };
                let old = cur[i as usize];
                let new = if round >= WIDEN { old.widen(f) } else { f };
                if new != old {
                    cur[i as usize] = new;
                    changed = true;
                }
            }
            if !changed {
                return Some(cur);
            }
        }
        None
    }

    /// Notes a legacy multiply left in its default form (`m` the product, `r` the select of
    /// +0 or it), for [`Module::legacy_after_joins`].
    pub fn legacy_site(&mut self, m: V, r: V, a: V, b: V) {
        self.legacy_sites.push([m, r, a, b]);
    }

    /// Once every join is filled, the legacy multiplies noted by [`Module::legacy_site`]
    /// whose operands' settled facts allow a shorter form (`choose`) take it in place: the
    /// select becomes the form's last instruction and the product its first; the compares
    /// are left to the clean-up. Returns how many changed.
    pub(crate) fn legacy_after_joins(&mut self, choose: impl Fn(&Facts, &Facts) -> Option<Form>) -> u32 {
        let sites = std::mem::take(&mut self.legacy_sites);
        if sites.is_empty() {
            return 0;
        }
        let Some(table) = self.settled() else {
            return 0;
        };
        let facts = |m: &Module, v: V| match v {
            V::Inst(j) => table[j as usize],
            _ => m.constant_facts(v).unwrap_or(Facts::ANY),
        };
        let mut n = 0;
        for [m, r, a, b] in sites {
            let (V::Inst(mi), V::Inst(ri)) = (m, r) else {
                continue;
            };
            let (fa, fb) = (facts(self, a), facts(self, b));
            let Some(form) = choose(&fa, &fb) else {
                continue;
            };
            let zero = self.f32(0);
            let (mop, rop) = match form {
                Form::Plain => (None, Op::Bin(Bin::Mul, a, b)),
                Form::AddFirst { swap } => {
                    let (x, k) = if swap { (b, a) } else { (a, b) };
                    (Some(Op::Bin(Bin::Add, x, zero)), Op::Bin(Bin::Mul, m, k))
                }
                Form::AddAfter => (None, Op::Bin(Bin::Add, m, zero)),
            };
            if let Some(op) = mop {
                self.insts[mi as usize].op = op;
            }
            self.insts[ri as usize].op = rop;
            n += 1;
        }
        n
    }

    /// An integer of at most 32 bits.
    fn narrow(&self, v: V) -> bool {
        matches!(self.types[self.type_of(v).0 as usize], TypeDef::Int(bits) if bits <= 32)
    }

    fn is_float_v(&self, v: V) -> bool {
        self.types[self.type_of(v).0 as usize] == TypeDef::Float
    }
}

/// DXIL's unary float operations.
fn unary(op: i64, x: Facts) -> Facts {
    match op {
        // FAbs
        6 => x.abs(),
        // Saturate
        7 => x.saturate(),
        // Cos, Sin: within [-1, 1] for numbers, NaN for infinities.
        12 | 13 => Facts {
            lo: -1.0,
            hi: 1.0,
            nan: !x.finite(),
            pz: true,
            nz: true,
            least: 0.0,
        },
        // Exp (base 2): never negative, never -0.
        21 => {
            let lo = 2f64.powf(x.lo);
            let hi = 2f64.powf(x.hi);
            let (lo, hi) = Facts::rounded(lo, hi);
            Facts {
                lo: lo.max(0.0),
                hi,
                nan: x.nan,
                pz: lo < NORMAL,
                nz: false,
                least: lo.max(NORMAL),
            }
        }
        // Frc: [0, 1] for numbers.
        22 => Facts {
            lo: 0.0,
            hi: 1.0,
            nan: !x.finite(),
            pz: true,
            nz: x.nz,
            least: 0.0,
        },
        // Sqrt: NaN below -0, -0 for -0.
        24 => {
            let (lo, hi) = Facts::rounded(x.lo.max(0.0).sqrt(), x.hi.max(0.0).sqrt());
            Facts {
                lo: lo.max(0.0),
                hi,
                nan: x.nan || x.lo < 0.0,
                pz: x.pz || lo < NORMAL,
                nz: x.nz,
                least: x.least.sqrt(),
            }
            .clear_zeros()
        }
        // Round_ne, Round_ni, Round_pi, Round_z: whole numbers.
        26..=29 => {
            let (lo, hi) = (x.lo.floor(), x.hi.ceil());
            let zero = lo < 1.0 && hi > -1.0;
            Facts {
                lo,
                hi,
                nan: x.nan,
                pz: zero,
                nz: zero && (x.nz || lo < 0.0),
                least: 1.0,
            }
        }
        _ => Facts::ANY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(x: f32) -> Facts {
        Facts::constant(x.to_bits())
    }

    #[test]
    fn products_and_sums_of_bounded_values_stay_finite() {
        let unit = k(0.0).join(k(1.0));
        assert!(unit.mul(k(1e30)).finite());
        assert!(!k(1e30).mul(k(1e30)).finite(), "past the largest float");
        assert!(k(3e38).add(k(3e38), false).hi.is_infinite());
        assert!(unit.add(k(-2.0), true).finite());
    }

    #[test]
    fn zeros_and_signs() {
        let u = Facts {
            lo: 0.0,
            hi: 4_294_967_296.0,
            nan: false,
            pz: true,
            nz: false,
            least: 1.0,
        };
        assert!(u.plus() && u.finite());
        assert!(u.mul(k(0.5)).plus(), "a clear sign times a positive number");
        assert!(!u.mul(k(-0.5)).plus());
        assert!(
            u.mul(k(-0.5)).minus() || u.mul(k(-0.5)).pz,
            "+0 times a negative is -0"
        );
        assert!(!u.neg().plus() && u.neg().minus() == !u.neg().pz);
        assert!(!k(-0.0).abs().nz);
        assert!(!k(f32::NAN).saturate().nan);
        assert!(k(0.25).sign_of_normal() == Some(true));
        assert!(k(-3.0).sign_of_normal() == Some(false));
        assert!(k(1e-40).sign_of_normal().is_none(), "a denormal is a zero");
        assert!(Facts::ANY.sign_of_normal().is_none());
    }

    #[test]
    fn the_legacy_product_of_a_zero_is_plus_zero() {
        let any = Facts::ANY;
        let l = k(0.0).legacy(any);
        assert!(l.nan, "a NaN operand stays NaN");
        let l = k(-0.0).legacy(k(f32::INFINITY));
        assert!(!l.nan && l.pz && !l.nz);
        let unit = k(0.0).join(k(1.0));
        assert!(unit.legacy(unit).finite());
    }

    #[test]
    fn a_reciprocal_of_a_converted_integer_is_positive() {
        let u = Facts {
            lo: 0.0,
            hi: 4_294_967_296.0,
            nan: false,
            pz: true,
            nz: false,
            least: 1.0,
        };
        let r = k(1.0).div(u);
        assert!(
            r.plus() && r.nonzero() && !r.nan && r.hi.is_infinite(),
            "{r:?}"
        );
        let r = k(-2.0).div(u);
        assert!(r.minus() && r.nonzero(), "{r:?}");
        assert_eq!(k(1.0).div(Facts::ANY), Facts::ANY);
        assert_eq!(Facts::ANY.div(u), Facts::ANY);
        // A divisor that may be infinite: the quotient may be a zero.
        let r = k(1.0).div(Facts {
            hi: f64::INFINITY,
            ..u
        });
        assert!(r.pz && !r.nz);
    }

    #[test]
    fn rounded_bounds_stay_on_floats() {
        // A product of numbers within [0, 1] stays within [0, 1] (so a product carried round
        // a loop settles), a bound between floats goes to the next one out, a bound under
        // the smallest normal may be a flushed zero, one past the largest an infinity.
        use crate::emit::lower::legacy::TEST_LOOPS;
        TEST_LOOPS.set(Some(true));
        assert_eq!(Facts::outward(0.0, 1.0), (0.0, 1.0));
        let (lo, hi) = Facts::outward(0.1, 0.1);
        assert!(lo < 0.1 && hi > 0.1 && (hi - lo) < 1e-8);
        assert_eq!(Facts::outward(1e-40, 1.0).0, 0.0);
        assert_eq!(Facts::outward(0.0, 1e39).1, f64::INFINITY);
        let unit = k(0.0).join(k(1.0));
        let p = unit.mul(unit);
        assert_eq!((p.lo, p.hi), (0.0, 1.0));
        // Without VICTORY_LEGACY_LOOPS, widened as before it.
        TEST_LOOPS.set(Some(false));
        let p = unit.mul(unit);
        assert!(p.hi > 1.0);
        TEST_LOOPS.set(None);
    }

    #[test]
    fn a_square_has_a_clear_sign() {
        let unit = k(-0.0).join(k(1.0));
        let sq = unit.mul(unit).square();
        assert!(sq.plus() && sq.finite());
        // 3 - 2x for x in [0, 1]: a positive normal number.
        let smooth = k(-2.0).mul(unit).add(k(3.0), false);
        assert_eq!(smooth.sign_of_normal(), Some(true), "{smooth:?}");
    }

    #[test]
    fn maximum_and_median_bounds() {
        let any = Facts::ANY;
        let m = any.max_min(k(0.0), true);
        assert!(
            !m.nan && m.lo >= 0.0 && !m.nz,
            "max(x, +0) is never -0 nor NaN"
        );
        let c = Facts::med3([any, k(0.0), k(1.0)]);
        assert!(c.finite() && c.lo == 0.0 && c.hi == 1.0);
    }
}
