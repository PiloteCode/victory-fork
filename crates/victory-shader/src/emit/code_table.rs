//! Where the table of a program's own code starts (`Lower::code_dword`
//! in the lowering): the constants the compiler appends to a program and
//! reads through `s_getpc_b64`.
//!
//! The table was the program's code from the lowest offset any load or
//! descriptor names, and a load at a computed offset names offset 0: the
//! table was then the whole program, instructions and all (3,200 to 4,600
//! dwords in the lighting computes reached at world entry). NVIDIA's
//! compiler keeps up to 16 KiB of such constant arrays in constant memory
//! and copies a larger one into every thread's local memory: on an
//! RTX 5090 under vkd3d-proton those computes then fault (Xid 13, shader
//! writes to unmapped addresses), and on other cards the driver reserves
//! gigabytes for them (Marvisak/VIctory#2143, desplmfao; brought here by
//! fxceless/victory-fork#1006, OctoAlgo).
//!
//! `VICTORY_CODE_TABLE_TAIL=1`: the table starts where the program's
//! instructions end. Off by default here: on Windows the only run reported
//! (an RTX 5080) showed the same picture, frame time and video memory with
//! and without it, so it waits for a measure on this machine's card.
//!
//! In this title's shader pack, as this translator lowers it (census of
//! 2026-11-25, 32-lane host): every one of the 25,953 programs translates
//! and validates either way, and 28 change with the switch, all compute:
//! 24 `CS_InterpolateStrands` (hair) and four `CS_ReconstructRaymarch*`.
//! Each loses 23 to 34 KiB of DXIL, the instructions its table held, and
//! each table was over the 16 KiB that NVIDIA's compiler keeps in
//! constant memory. Unset, all 25,953 are byte for byte as before.

use crate::ir::Program;
use crate::resource::Address;

/// Whether the table starts after the instructions
/// (`VICTORY_CODE_TABLE_TAIL=1`).
pub(super) fn tail_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_CODE_TABLE_TAIL").is_some_and(|v| v == "1"))
}

/// Where a program's constant table starts in its code: where its
/// instructions end, unless a load at a fixed offset reads below that (then
/// at `lowest`, the lowest offset any load or descriptor names). The
/// program's constants follow its instructions, and a load at a computed
/// offset reads them, not instruction bytes. Never below `lowest`, so the
/// table is never larger than it was.
pub(super) fn start(p: &Program, addresses: &[Address], lowest: u32) -> u32 {
    let code_end = p
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .map(|i| i.machine.offset + 4 * i.machine.words.len())
        .max()
        .unwrap_or(0) as u32;
    let fixed_below = addresses.iter().any(|a| a.dynamic.is_none() && a.offset < code_end);
    if fixed_below { lowest } else { lowest.max(code_end & !3) }
}

#[cfg(test)]
mod tests {
    use super::start;
    use crate::{ir, resource};

    const S_ENDPGM: u32 = 0xBF81_0000;

    /// A program's constant table starts after its instructions, unless a
    /// load at a fixed offset reads below that.
    #[test]
    fn the_code_table_starts_after_the_instructions() {
        // s_nop 0, s_endpgm, then 1,000 dwords of constants.
        let mut words = vec![0xBF80_0000, S_ENDPGM];
        words.extend(0..1000u32);
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let p = ir::build(&code, false, 256).unwrap();
        let at = |offset: u32, dynamic: bool| resource::Address {
            param: resource::CODE,
            offset,
            dynamic: dynamic.then_some(ir::Value(0)),
        };
        let (computed, fixed_after, fixed_inside) = (at(0, true), at(12, false), at(4, false));
        // A load at a computed offset from the code's start: the table is
        // the constants only, not the whole program.
        assert_eq!(start(&p, &[computed], 0), 8);
        assert_eq!(start(&p, &[computed, fixed_after], 0), 8);
        // A load at a fixed offset inside the instructions keeps the start.
        assert_eq!(start(&p, &[computed, fixed_inside], 0), 0);
        // Loads past the instructions only: from the lowest of them.
        assert_eq!(start(&p, &[fixed_after], 12), 12);
    }
}
