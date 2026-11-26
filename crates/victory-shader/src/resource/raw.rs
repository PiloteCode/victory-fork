//! A buffer descriptor made raw before byte accesses, as the FidelityFX
//! upscaler's compiler does (after avatarmanz's translator, 66afb2b4).

use super::{Term, Tracer};
use crate::ir::Value;

/// `s_bfe_u32`'s field operand for a buffer descriptor's stride: offset 16,
/// width 14 (dword 1 bits 16-29).
const STRIDE_FIELD: u32 = 14 << 16 | 16;

impl Tracer<'_> {
    /// The words of the buffer descriptor that `words` makes raw: the stride
    /// (dword 1 bits 16-29) cleared, the record count times max(stride, 1),
    /// out-of-bounds checking by bytes (dword 3 bits 28-29 set). The bounds
    /// are the view's size in bytes either way, and the host's views of
    /// buffers are raw: an access through the raw words reads what it reads
    /// through the loaded ones.
    pub(super) fn made_raw(&self, words: &[Term]) -> Option<[Term; 4]> {
        let [w0, Term::Value(w1), Term::Value(w2), Term::Value(w3)] = *words else {
            return None;
        };
        // `x op c` with `c` either side.
        let with = |v: Value, name: &str, c: u32| match self.sop2(v, &[name])?.1 {
            [x, Term::Const(k)] | [Term::Const(k), x] if k == c => Some(x),
            _ => None,
        };
        let d1 = with(w1, "s_and_b32", 0xC000_FFFF)?;
        let Term::Value(m3) = with(w3, "s_or_b32", 0x3000_0000)? else {
            return None;
        };
        let d3 = with(m3, "s_and_b32", 0xCFFF_FFFF)?;
        let [a, b] = self.sop2(w2, &["s_mul_i32"])?.1;
        let max = |t: Term| match t {
            Term::Value(s) => with(s, "s_max_u32", 1),
            Term::Const(_) => None,
        };
        let (d2, field) = match (max(a), max(b)) {
            (_, Some(f)) => (a, f),
            (Some(f), None) => (b, f),
            (None, None) => return None,
        };
        let Term::Value(field) = field else {
            return None;
        };
        let (_, [x, Term::Const(STRIDE_FIELD)]) = self.sop2(field, &["s_bfe_u32"])? else {
            return None;
        };
        (x == d1).then_some([w0, d1, d2, d3])
    }
}

#[cfg(test)]
mod tests {
    use crate::ShaderError;
    use crate::native::{HwStage, RootParam, RootType};

    const S_ENDPGM: u32 = 0xBF81_0000;

    /// The upscaler's byte accesses make a loaded buffer descriptor raw
    /// first (stride cleared, records times stride, bounds in bytes): the
    /// access reads the loaded descriptor. Any other change still fails.
    #[test]
    fn a_descriptor_made_raw_is_the_loaded_one() {
        let words = |mask: u32| -> Vec<u8> {
            [
                0xBE88_0304, // s_mov_b32 s8, s4
                0xBE89_0380, // s_mov_b32 s9, 0
                0xF408_0204,
                0xFA00_0160, // s_load_dwordx4 s[8:11], s[8:9], 0x160
                0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
                0x938C_FF09,
                0x000E_0010, // s_bfe_u32 s12, s9, 0x000e0010
                0x848C_810C, // s_max_u32 s12, s12, 1
                0x8709_FF09,
                mask, // s_and_b32 s9, s9, mask
                0x870B_FF0B,
                0xCFFF_FFFF, // s_and_b32 s11, s11, 0xcfffffff
                0x930A_0C0A, // s_mul_i32 s10, s10, s12
                0x880B_FF0B,
                0x3000_0000, // s_or_b32 s11, s11, 0x30000000
                0xE078_1000,
                0x8002_0007, // buffer_store_dwordx4 v[0:3], v7, s[8:11], 0 offen
                S_ENDPGM,
            ]
            .iter()
            .flat_map(|w: &u32| w.to_le_bytes())
            .collect()
        };
        let layout = [RootParam {
            root: RootType::Table,
            table_offset_dwords: 0,
            sgprs: 1,
            base_register: 0,
            user_data: 4,
            slot: 0,
        }];
        let run = |mask| {
            let p = crate::ir::build(&words(mask), false, 256).unwrap();
            let lanes = crate::structure::structure(&p).ok();
            crate::resource::track(&p, &layout, HwStage::Cs, lanes.as_ref())
        };
        assert!(run(0xC000_FFFF).is_ok());
        assert_eq!(
            run(0xC000_FFFE).unwrap_err(),
            ShaderError::Unsupported {
                offset: 0x3c,
                what: "a descriptor changed after loading"
            }
        );
    }
}
