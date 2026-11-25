//! Cheaper exact lowerings of common console instructions, each behind a
//! switch of its own and off by default. They are part of the translation,
//! so the shader cache keys on them (`victory-gpu`'s does). (#995's
//! `VICTORY_QUAD_OPS` is not here: `quad.rs` already makes DPP quad
//! permutations host quad operations, `VICTORY_QUAD_READS`, on by default.)
//!
//! - `VICTORY_CLASS_RANGES=1`: `v_cmp_class_f32/f16` with a constant class
//!   mask tests the ranges of the magnitude's bits those classes cover
//!   (zero, denormals, normals, infinity, signalling and quiet NaNs lie in
//!   that order), with the sign where the mask asks for one: `±inf` is two
//!   instructions instead of some twenty (the general form works out the
//!   class number and shifts the mask by it).

use super::bitcode::{Bin, Module, V, pred};

/// Whether class tests with a constant mask test ranges
/// (`VICTORY_CLASS_RANGES=1`).
pub(super) fn class_ranges_on() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_CLASS_RANGES.get() {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_CLASS_RANGES").is_some_and(|v| v == "1"))
}

#[cfg(test)]
thread_local! {
    /// A test's choices for the switches, for its thread only.
    pub(super) static TEST_CLASS_RANGES: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// A run of the magnitude's bits `lo..=hi` and the sign it needs (`None`:
/// either).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Run {
    pub lo: u32,
    pub hi: u32,
    pub negative: Option<bool>,
}

/// The magnitudes of each class of a float, in their order: zero,
/// denormals, normals, infinity, signalling NaNs, quiet NaNs; with the mask
/// bits of the positive and the negative class.
fn kinds(f16: bool) -> [(u32, u32, u32, u32); 6] {
    let (inf, quiet, top) = if f16 { (0x7C00, 0x7E00, 0x7FFF) } else { (0x7F80_0000, 0x7FC0_0000, 0x7FFF_FFFF) };
    let first_normal = if f16 { 0x0400 } else { 0x0080_0000 };
    [
        (0, 0, 6, 5),
        (1, first_normal - 1, 7, 4),
        (first_normal, inf - 1, 8, 3),
        (inf, inf, 9, 2),
        (inf + 1, quiet - 1, 0, 0),
        (quiet, top, 1, 1),
    ]
}

/// The runs of magnitudes a class mask (bits 0-9: signalling NaN, quiet
/// NaN, -inf, -normal, -denormal, -0, +0, +denormal, +normal, +inf) takes,
/// adjacent classes with the same sign rule joined.
pub(super) fn class_runs(mask: u32, f16: bool) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (lo, hi, pos, neg) in kinds(f16) {
        let (p, n) = (mask >> pos & 1 != 0, mask >> neg & 1 != 0);
        let negative = match (p, n) {
            (false, false) => continue,
            (true, true) => None,
            (true, false) => Some(false),
            (false, true) => Some(true),
        };
        match runs.last_mut() {
            Some(r) if r.hi + 1 == lo && r.negative == negative => r.hi = hi,
            _ => runs.push(Run { lo, hi, negative }),
        }
    }
    runs
}

/// The class test of `x` (an f32's bits, or an f16's in the low half) for
/// the constant `mask`, as the general form decides it: the magnitude is
/// the bits below the sign, the sign is anything at or above it.
pub(super) fn class_test(m: &mut Module, c: usize, x: V, mask: u32, f16: bool) -> V {
    let runs = class_runs(mask, f16);
    let (top, sign_shift) = if f16 { (0x7FFF, 15) } else { (0x7FFF_FFFF, 31) };
    if runs.is_empty() {
        return m.bool(false);
    }
    let k = |m: &mut Module, v: u32| m.i32(v);
    let km = k(m, top);
    let abs = m.bin(c, Bin::And, x, km);
    let mut sign = None;
    let mut result: Option<V> = None;
    for run in runs {
        let range = match (run.lo, run.hi) {
            (0, hi) if hi == top => None,
            (lo, hi) if lo == hi => {
                let v = k(m, lo);
                Some(m.cmp(c, pred::EQ, abs, v))
            }
            (0, hi) => {
                let v = k(m, hi);
                Some(m.cmp(c, pred::IULE, abs, v))
            }
            (lo, hi) if hi == top => {
                let v = k(m, lo);
                Some(m.cmp(c, pred::IUGE, abs, v))
            }
            (lo, hi) => {
                let (l, n) = (k(m, lo), k(m, hi - lo));
                let off = m.bin(c, Bin::Sub, abs, l);
                Some(m.cmp(c, pred::IULE, off, n))
            }
        };
        let signed = run.negative.map(|negative| {
            let s = *sign.get_or_insert_with(|| {
                let sh = k(m, sign_shift);
                let s = m.bin(c, Bin::LShr, x, sh);
                let z = k(m, 0);
                m.cmp(c, pred::NE, s, z)
            });
            if negative {
                s
            } else {
                let t = m.bool(true);
                m.bin(c, Bin::Xor, s, t)
            }
        });
        let term = match (range, signed) {
            (Some(a), Some(b)) => m.bin(c, Bin::And, a, b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => m.bool(true),
        };
        result = Some(match result {
            Some(r) => m.bin(c, Bin::Or, r, term),
            None => term,
        });
    }
    result.unwrap_or_else(|| m.bool(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The general form's class number of `x` (`Lower::compare`).
    fn class_of(x: u32, f16: bool) -> u32 {
        let (exp_sh, sign_sh, exp_mask, mant_mask, quiet_bit) =
            if f16 { (10, 15, 0x1F, 0x03FF, 0x0200) } else { (23, 31, 0xFF, 0x7F_FFFF, 0x40_0000) };
        let e = x >> exp_sh & exp_mask;
        let m = x & mant_mask;
        let neg = x >> sign_sh != 0;
        let t = if m == 0 { 6 } else { 7 };
        let t = if e == 0 { t } else { 8 };
        let t = if e == exp_mask { 9 } else { t };
        let t = if neg { 11 - t } else { t };
        if m != 0 && e == exp_mask { u32::from(x & quiet_bit != 0) } else { t }
    }

    /// `x` against the runs, as [`class_test`] builds it.
    fn runs_say(runs: &[Run], x: u32, f16: bool) -> bool {
        let (top, sign_shift) = if f16 { (0x7FFF, 15) } else { (0x7FFF_FFFF, 31) };
        let abs = x & top;
        let neg = x >> sign_shift != 0;
        runs.iter().any(|r| abs >= r.lo && abs <= r.hi && r.negative.is_none_or(|n| n == neg))
    }

    #[test]
    fn runs_agree_with_the_general_form_for_every_half_and_mask() {
        for mask in 0..1024u32 {
            let runs = class_runs(mask, true);
            for x in 0..=0xFFFFu32 {
                let want = mask >> class_of(x, true) & 1 != 0;
                assert_eq!(runs_say(&runs, x, true), want, "mask {mask:#x} x {x:#06x}");
            }
            // The f16 form reads bits above the half as a sign, as the
            // general one does.
            for x in [0x1_0000u32, 0x1_3C00, 0xFFFF_7C00] {
                let want = mask >> class_of(x, true) & 1 != 0;
                assert_eq!(runs_say(&runs, x, true), want, "mask {mask:#x} x {x:#x}");
            }
        }
    }

    #[test]
    fn runs_agree_with_the_general_form_for_floats() {
        // Every class edge and its neighbours, both signs.
        let mut xs = Vec::new();
        for edge in [0u32, 1, 0x7F_FFFF, 0x80_0000, 0x3F80_0000, 0x7F7F_FFFF, 0x7F80_0000, 0x7F80_0001, 0x7FBF_FFFF,
            0x7FC0_0000, 0x7FFF_FFFF]
        {
            for d in [0u32, 1, 2] {
                for x in [edge.wrapping_sub(d), edge.wrapping_add(d)] {
                    xs.push(x & 0x7FFF_FFFF);
                    xs.push(x | 0x8000_0000);
                }
            }
        }
        let mut seed = 0x9E37_79B9u32;
        for _ in 0..4096 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            xs.push(seed);
        }
        for mask in 0..1024u32 {
            let runs = class_runs(mask, false);
            for &x in &xs {
                let want = mask >> class_of(x, false) & 1 != 0;
                assert_eq!(runs_say(&runs, x, false), want, "mask {mask:#x} x {x:#010x}");
            }
        }
    }

    #[test]
    fn common_masks_are_one_run() {
        let one = |mask, f16| class_runs(mask, f16).len();
        // ±inf, NaN, ±0, ±denormal, finite, not NaN.
        for mask in [0x204, 0x003, 0x060, 0x090, 0x1F8, 0x3FC] {
            assert_eq!(one(mask, false), 1, "{mask:#x}");
        }
        assert_eq!(class_runs(0x204, false), [Run { lo: 0x7F80_0000, hi: 0x7F80_0000, negative: None }]);
        assert_eq!(class_runs(0x200, true), [Run { lo: 0x7C00, hi: 0x7C00, negative: Some(false) }]);
        assert!(class_runs(0, false).is_empty());
        assert_eq!(class_runs(0x3FF, false), [Run { lo: 0, hi: 0x7FFF_FFFF, negative: None }]);
    }
}
