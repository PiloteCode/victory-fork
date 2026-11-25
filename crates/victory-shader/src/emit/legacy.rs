//! Cheaper exact forms of the legacy multiply (a child module of `lower`, sharing the
//! emitter's state).
//!
//! `v_mul_legacy_f32` (and the multiply of `v_mac_legacy_f32` and `v_mad_legacy_f32`) gives
//! +0 where either operand is a zero (a denormal read as one), whatever the other is,
//! infinity and NaN included, and the IEEE product otherwise. The default form is the host's
//! product, a compare of each operand with zero, their OR and a select (`Lower::legacy_mul`).
//! Where what is known of the operands ([`Facts`], `bitcode::facts`) makes a shorter form
//! give the same bits for every input, the multiply takes that form:
//!
//! - The product alone (`VICTORY_LEGACY_PLAIN`, on; `=0` turns it off): where every zero
//!   either operand may be meets a finite other whose sign makes the IEEE product +0: a +0
//!   only finite numbers of clear sign (+0 included), a -0 only finite numbers of set sign.
//!   Then each zero case is +0 by IEEE, and every other case is the product as before. (A
//!   converted unsigned integer times a positive constant, the title's most frequent such
//!   pair; the sum or product of such values.)
//! - With `VICTORY_LEGACY_ZERO=1` (off), forms that make the zero's +0 with an add of +0,
//!   exact by IEEE's rule that a sum of two zeros of opposite signs is +0 when rounding to
//!   nearest; they hold where the host keeps `x + 0.0` (DXIL arithmetic without fast-math
//!   flags is precise; the switch stays off until the card is measured to keep it):
//!   - one operand a finite number of at least the smallest normal magnitude and positive
//!     (a constant, mostly): `(x + 0) * k`. A zero x becomes +0, +0 times a positive number
//!     is +0; any other x is unchanged by the add, so the product is the IEEE product; an
//!     infinite or NaN x gives what the product gives, k being neither zero nor infinite;
//!   - a negative constant k: `(0 - x) * -k`, the same with the signs of both factors turned
//!     (0 - x is +0 for either zero and -x otherwise, exactly);
//!   - both operands finite, one a whole number wherever it is not a zero (a converted
//!     integer, a rounded value): `x * y + 0`. A zero operand makes a zero product, which the
//!     add makes +0; two others make a product at least as large as the other operand,
//!     never a zero (at least the smallest normal, or the smallest denormal on a host that
//!     keeps them), which the add leaves as it is.
//!
//! Each switch is part of the translation (the shader cache keys on it); with both off each
//! program is as before, bit for bit.

use super::{Bin, Lower, V};
use crate::emit::bitcode::facts::{Facts, Made};

/// The product alone where it is exact without a test (`VICTORY_LEGACY_PLAIN=0`: never).
pub(super) fn plain_on() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_PLAIN.get() {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LEGACY_PLAIN").is_none_or(|v| v != "0"))
}

/// The forms that add +0 (`VICTORY_LEGACY_ZERO=1`; off by default).
pub(super) fn zero_on() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_ZERO.get() {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LEGACY_ZERO").is_some_and(|v| v == "1"))
}

#[cfg(test)]
thread_local! {
    /// A test's choice for [`plain_on`], for its thread only.
    pub(crate) static TEST_PLAIN: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    /// A test's choice for [`zero_on`], for its thread only.
    pub(crate) static TEST_ZERO: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Which shorter form a legacy multiply of operands with these facts takes, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Form {
    /// `a * b`.
    Plain,
    /// `(a + 0) * b`, b positive (or with the operands swapped: `(b + 0) * a`).
    AddFirst { swap: bool },
    /// `(0 - a) * -b`, b a negative constant (or swapped).
    SubFirst { swap: bool },
    /// `a * b + 0`.
    AddAfter,
}

/// The form for operands with facts `fa` and `fb` (`b_constant`, `a_constant`: whether each
/// is a constant), under the switches as given.
pub(super) fn choose(
    fa: &Facts,
    fb: &Facts,
    a_constant: bool,
    b_constant: bool,
    plain: bool,
    zero: bool,
) -> Option<Form> {
    // A zero of `x` makes +0 against every value `y` may take.
    let zeros_fine = |x: &Facts, y: &Facts| {
        (!x.pz || (y.finite() && y.plus())) && (!x.nz || (y.finite() && y.minus()))
    };
    if plain && zeros_fine(fa, fb) && zeros_fine(fb, fa) {
        return Some(Form::Plain);
    }
    if !zero {
        return None;
    }
    for (swap, k, k_constant) in [(false, fb, b_constant), (true, fa, a_constant)] {
        match k.sign_of_normal() {
            Some(true) => return Some(Form::AddFirst { swap }),
            Some(false) if k_constant => return Some(Form::SubFirst { swap }),
            _ => {}
        }
    }
    if fa.finite() && fb.finite() && (fa.least >= 1.0 || fb.least >= 1.0) {
        return Some(Form::AddAfter);
    }
    None
}

impl Lower<'_> {
    /// The legacy multiply of `a` and `b` in a shorter exact form, where one applies (see the
    /// module's notes); `None` leaves it to the default form.
    pub(super) fn legacy_short(&mut self, a: V, b: V) -> Option<V> {
        let (plain, zero) = (plain_on(), zero_on());
        if !plain && !zero {
            return None;
        }
        let (fa, fb) = (self.s.m.facts(a), self.s.m.facts(b));
        let (ka, kb) = (self.s.m.float_bits(a), self.s.m.float_bits(b));
        let form = choose(&fa, &fb, ka.is_some(), kb.is_some(), plain, zero)?;
        let c = self.cur;
        let r = match form {
            Form::Plain => self.s.m.bin(c, Bin::Mul, a, b),
            Form::AddFirst { swap } => {
                let (x, k) = if swap { (b, a) } else { (a, b) };
                let zero = self.s.m.f32(0);
                let x = self.s.m.bin(c, Bin::Add, x, zero);
                self.s.m.bin(c, Bin::Mul, x, k)
            }
            Form::SubFirst { swap } => {
                let x = if swap { b } else { a };
                let bits = if swap { ka } else { kb }.expect("a constant");
                let zero = self.s.m.f32(0);
                let x = self.s.m.bin(c, Bin::Sub, zero, x);
                let k = self.s.m.f32(bits ^ 0x8000_0000);
                self.s.m.bin(c, Bin::Mul, x, k)
            }
            Form::AddAfter => {
                let m = self.s.m.bin(c, Bin::Mul, a, b);
                let zero = self.s.m.f32(0);
                self.s.m.bin(c, Bin::Add, m, zero)
            }
        };
        #[cfg(test)]
        TEST_SHORT.set(TEST_SHORT.get() + 1);
        self.s.m.made(r, Made::Legacy(a, b));
        Some(r)
    }
}

#[cfg(test)]
thread_local! {
    /// The legacy multiplies lowered in a shorter form on this thread.
    pub(crate) static TEST_SHORT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(x: f32) -> Facts {
        Facts::constant(x.to_bits())
    }

    const UINT: Facts = Facts {
        lo: 0.0,
        hi: 4_294_967_296.0,
        nan: false,
        pz: true,
        nz: false,
        least: 1.0,
    };

    #[test]
    fn forms_by_what_is_known() {
        let any = Facts::ANY;
        assert_eq!(
            choose(&UINT, &k(0.5), false, true, true, false),
            Some(Form::Plain)
        );
        assert_eq!(
            choose(&k(0.5), &UINT, true, false, true, false),
            Some(Form::Plain)
        );
        // +0 times -0.5 is -0: not the product alone.
        assert_eq!(choose(&UINT, &k(-0.5), false, true, true, false), None);
        assert_eq!(
            choose(&UINT, &k(-0.5), false, true, true, true),
            Some(Form::SubFirst { swap: false })
        );
        assert_eq!(choose(&any, &k(0.5), false, true, true, false), None);
        assert_eq!(
            choose(&any, &k(0.5), false, true, true, true),
            Some(Form::AddFirst { swap: false })
        );
        assert_eq!(
            choose(&k(3.0), &any, true, false, true, true),
            Some(Form::AddFirst { swap: true })
        );
        // Infinity, NaN, zero and denormal constants: no short form.
        for x in [f32::INFINITY, f32::NAN, 0.0, -0.0, 1e-40] {
            assert_eq!(choose(&any, &k(x), false, true, true, true), None, "{x}");
        }
        // A finite value times a whole number.
        let unit = Facts {
            lo: 0.0,
            hi: 1.0,
            nan: false,
            pz: true,
            nz: true,
            least: 0.0,
        };
        assert_eq!(
            choose(&unit, &UINT, false, false, true, true),
            Some(Form::AddAfter)
        );
        assert_eq!(choose(&unit, &UINT, false, false, true, false), None);
        assert_eq!(
            choose(&unit, &unit, false, false, true, true),
            None,
            "a product may underflow"
        );
        assert_eq!(choose(&any, &any, false, false, true, true), None);
        assert_eq!(
            choose(&UINT, &k(0.5), false, true, false, false),
            None,
            "both switches off"
        );
    }
}
