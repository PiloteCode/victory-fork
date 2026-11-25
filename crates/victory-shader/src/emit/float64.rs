//! 64-bit floats as the host's doubles: `v_add_f64`, `v_mul_f64`, `v_cvt_f32_f64`,
//! `v_cvt_f64_f32`, and the 64-bit float wave sums of whole-wave sections
//! ([`super::super::wave::Shape::Sum64`]). The title's water volumetrics count their lanes in
//! doubles (`WaveActiveSum(1.0L)`, `v_add_f64` over DPP-moved dword pairs) and take the count
//! back as a float.
//!
//! The host's doubles are IEEE, rounded to nearest with denormals kept, as the console's FP64
//! mode is. A double is made from its two dwords (`dx.op.makeDouble`) and taken apart again
//! (`dx.op.splitDouble`), so no 64-bit integer is needed.

use super::{Attr, Bin, Cast, Fields, Inst, Lower, MaskData, Operand, Reg, V, Value, Why, flags, need};
use crate::gfx10::NULL;

impl Lower<'_> {
    /// The 64-bit float operations, if `name` is one.
    pub(super) fn valu_f64(&mut self, inst: &Inst, name: &str, guard: Option<V>) -> Option<Result<(), Why>> {
        let c = self.cur;
        Some(match name {
            "v_cvt_f32_f64" => self.fsrc64(inst, 0).and_then(|x| {
                let r = self.s.m.cast(c, Cast::FpTrunc, x, self.s.t.f32);
                self.write_f(inst, r, guard)
            }),
            "v_cvt_f64_f32" => self.fsrc(inst, 0).and_then(|x| {
                let f64t = self.s.m.double();
                self.s.flags |= flags::DOUBLES;
                let r = self.s.m.cast(c, Cast::FpExt, x, f64t);
                self.write_f64(inst, r, guard)
            }),
            "v_add_f64" | "v_mul_f64" => (|| {
                let (a, b) = (self.fsrc64(inst, 0)?, self.fsrc64(inst, 1)?);
                let op = if name == "v_add_f64" { Bin::Add } else { Bin::Mul };
                let r = self.s.m.bin(c, op, a, b);
                self.write_f64(inst, r, guard)
            })(),
            _ => return None,
        })
    }

    /// The 64-bit float `WaveActiveSum` over the console's wave of each lane's own
    /// `mask ? (lo, hi) : 0.0`, as its two dwords: the host's wave operation on doubles, and
    /// on two host waves of 32 their sums added (the lower half's first). Made once per
    /// block, for both dwords.
    /// shortcut: the host adds a wave's terms in its own order, not the console's tree of
    /// lanes; the sum can differ in its last bits where terms round (as for 32-bit sums).
    pub(super) fn sum64(&mut self, lo: Value, hi: Value, mask: Value) -> Result<[V; 2], Why> {
        let key = (lo, hi, mask, self.cur);
        if let Some(&d) = self.sums64.get(&key) {
            return Ok(d);
        }
        if self.mask_data == MaskData::None {
            return Err("a wave reduction in a mesh shader");
        }
        let c = self.cur;
        let &m = self.x.st.masks.get(&mask).ok_or("a wave reduction's untracked mask")?;
        let on = self.boolean(m, c)?;
        let (l, h) = (self.i32_of(lo)?, self.i32_of(hi)?);
        let x = self.masked_double(on, l, h);
        let r = self.wave_sum_f64(x);
        // two host waves of 32 hold the console's 64 lanes: their sums add up
        let r = if self.local || self.exchange.as_ref().is_some_and(|e| e.1) {
            r
        } else if self.exchange.is_some() {
            if !self.all_lanes(self.cur_ir) {
                need(self.cur_ir);
                return Err("a wave reduction where not every lane runs");
            }
            let (rl, rh) = self.split_double(r);
            let c = self.cur;
            let (lower_lo, upper_lo) = self.exchange_halves(c, rl);
            let (lower_hi, upper_hi) = self.exchange_halves(c, rh);
            let lower = self.make_double(lower_lo, lower_hi);
            let upper = self.make_double(upper_lo, upper_hi);
            self.s.m.bin(self.cur, Bin::Add, lower, upper)
        } else {
            r
        };
        let (dl, dh) = self.split_double(r);
        self.sums64.insert(key, [dl, dh]);
        Ok([dl, dh])
    }

    /// [`Self::sum64`] in the paired form: the lower lanes' sum, then the upper lanes' added,
    /// as two host waves add theirs; dword `high` of it, for both lanes of this thread.
    pub(super) fn paired_sum64(&mut self, high: bool, lo: Value, hi: Value, mask: Value) -> Result<[V; 2], Why> {
        let key = (lo, hi, mask, self.cur);
        let d = match self.sums64.get(&key) {
            Some(&d) => d,
            None => {
                let &m = self.x.st.masks.get(&mask).ok_or("a wave reduction's untracked mask")?;
                let c = self.cur;
                let [x0, x1] = self.both(|l| {
                    let on = l.boolean(m, c)?;
                    let (dl, dh) = (l.i32_of(lo)?, l.i32_of(hi)?);
                    Ok(l.masked_double(on, dl, dh))
                })?;
                let (s0, s1) = (self.wave_sum_f64(x0), self.wave_sum_f64(x1));
                let total = self.s.m.bin(self.cur, Bin::Add, s0, s1);
                let (dl, dh) = self.split_double(total);
                self.sums64.insert(key, [dl, dh]);
                [dl, dh]
            }
        };
        let x = d[usize::from(high)];
        Ok([x, x])
    }

    /// `on ? (lo, hi) : +0.0` as a double.
    fn masked_double(&mut self, on: V, lo: V, hi: V) -> V {
        let x = self.make_double(lo, hi);
        let z = self.s.m.i32(0);
        let zero = self.make_double(z, z);
        self.s.m.select(self.cur, on, x, zero)
    }

    /// `WaveActiveSum` of the double `x` over this host wave's active lanes.
    fn wave_sum_f64(&mut self, x: V) -> V {
        self.s.flags |= flags::WAVE_OPS;
        let c = self.cur;
        let f64t = self.s.m.double();
        let (sum, signed) = (self.s.m.i8(0), self.s.m.i8(0));
        self.s.op(c, "waveActiveOp.f64", Attr::NoUnwind, f64t, 119, &[x, sum, signed])
    }

    /// The double whose dwords are `lo` and `hi` (`dx.op.makeDouble`).
    fn make_double(&mut self, lo: V, hi: V) -> V {
        let c = self.cur;
        let f64t = self.s.m.double();
        self.s.flags |= flags::DOUBLES;
        self.s.op(c, "makeDouble.f64", Attr::ReadNone, f64t, 101, &[lo, hi])
    }

    /// The low and high dwords of the double `d` (`dx.op.splitDouble`).
    fn split_double(&mut self, d: V) -> (V, V) {
        let c = self.cur;
        let i32t = self.s.t.i32;
        let pair = self.s.m.named_struct("dx.types.splitdouble", vec![i32t, i32t]);
        let r = self.s.op(c, "splitDouble.f64", Attr::ReadNone, pair, 102, &[d]);
        (self.s.m.extract(c, r, 0), self.s.m.extract(c, r, 1))
    }

    /// A 64-bit float source: a register pair, or an inline constant (a float one is that
    /// double, the integer 0 is +0.0), with its sign modifiers (on the high dword's sign).
    fn fsrc64(&mut self, inst: &Inst, k: usize) -> Result<V, Why> {
        let Fields::Valu(v) = &inst.fields else {
            unreachable!()
        };
        if v.sdwa.is_some() || v.dpp.is_some() {
            return Err("64-bit operand modifiers");
        }
        let op = *v.src.get(k).ok_or("a missing operand")?;
        let pair = |l: &mut Self, r: Reg, r1: Reg| -> Result<(V, V), Why> {
            let lo = l.i32_of(inst.read(r).ok_or("an unread operand")?)?;
            let hi = l.i32_of(inst.read(r1).ok_or("an unread operand")?)?;
            Ok((lo, hi))
        };
        let (lo, hi) = match op {
            Operand::Vector { reg, .. } => pair(self, Reg::V(reg), Reg::V(reg + 1))?,
            Operand::Scalar { code, .. } if code != NULL => pair(self, Reg::S(code), Reg::S(code + 1))?,
            Operand::Const(c) => {
                let x: f64 = match c {
                    128 => 0.0,
                    240 => 0.5,
                    241 => -0.5,
                    242 => 1.0,
                    243 => -1.0,
                    244 => 2.0,
                    245 => -2.0,
                    246 => 4.0,
                    247 => -4.0,
                    // 1 / (2 pi), the double the hardware holds
                    248 => f64::from_bits(0x3FC4_5F30_6DC9_C882),
                    _ => return Err("a 64-bit float inline constant"),
                };
                let bits = x.to_bits();
                (self.s.m.i32(bits as u32), self.s.m.i32((bits >> 32) as u32))
            }
            _ => return Err("a 64-bit float literal"),
        };
        let c = self.cur;
        let mut hi = hi;
        if v.abs >> k & 1 != 0 {
            let m = self.s.m.i32(0x7FFF_FFFF);
            hi = self.s.m.bin(c, Bin::And, hi, m);
        }
        if v.neg >> k & 1 != 0 {
            let m = self.s.m.i32(0x8000_0000);
            hi = self.s.m.bin(c, Bin::Xor, hi, m);
        }
        Ok(self.make_double(lo, hi))
    }

    /// A 64-bit float result into its VGPR pair.
    fn write_f64(&mut self, inst: &Inst, d: V, guard: Option<V>) -> Result<(), Why> {
        let Fields::Valu(v) = &inst.fields else {
            unreachable!()
        };
        if v.clamp || v.omod != 0 {
            return Err("64-bit float output modifiers");
        }
        let Some(Operand::Vector { reg, .. }) = v.dst else {
            return Err("a 64-bit result");
        };
        let (lo, hi) = self.split_double(d);
        self.write(inst, Reg::V(reg), lo, guard);
        self.write(inst, Reg::V(reg + 1), hi, guard);
        Ok(())
    }
}
