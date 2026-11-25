//! Changes taken from the official repository (Marvisak/VIctory) after ffe310eb, the commit our
//! fork's translator started from, a child module of the lowering so they share the emitter's
//! state. Each names its pull request or commit there.
//!
//! The ones that change a title program's translation have a switch, `VICTORY_<NAME>=0` to turn
//! it off (on by default), part of the translation the shader cache keys on (`build.rs` lists
//! every `VICTORY_*` name of this crate). The lowerings of instructions the title never runs
//! have none: no translation of the title changes with them.

use super::{
    Attr, Bin, Cast, Fields, Inst, Lower, MaskData, Operand, Program, Reg, V, Val, Value, Why, pred,
};

/// Whether a port is on: its variable unset, or anything but `0`.
fn on(cell: &'static std::sync::OnceLock<bool>, name: &str) -> bool {
    *cell.get_or_init(|| std::env::var(name).map_or(true, |v| v.trim() != "0"))
}

/// A VOP3 output multiplier (`mul:2`, `mul:4`, `div:2`) reads its operand's denormal as a zero
/// of its sign and gives +0 for a zero or denormal product, as RDNA2 does with IEEE mode off
/// (measured on gfx1031 and gfx1036 by the Leonida project; official 77053a0c). Ours multiplies
/// on the host, where a zero product keeps its sign. Off by default, `VICTORY_OMOD_ZERO=1` to
/// turn it on: compilers fold every x2, x4 and x0.5 into an output multiplier, so it adds eight
/// instructions to 12,276 programs of the title, for the sign of a zero.
pub(super) fn omod_zero() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VICTORY_OMOD_ZERO").is_ok_and(|v| v.trim() == "1"))
}

/// `s_min_*` / `s_max_*` set SCC to whether the first operand was chosen (RDNA2 ISA), where
/// ours set it to whether the result was non-zero. From the official 77053a0c.
/// `VICTORY_SMINMAX_SCC=0`: as before.
pub(super) fn sminmax_scc() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    on(&ON, "VICTORY_SMINMAX_SCC")
}

/// `v_interp_p2_f16` honours op_sel bit 3 (its result in the high half of the register, the
/// low half kept), omod and clamp; ours wrote the low half without the modifiers. From the
/// official #1696 (e29391f5). `VICTORY_INTERP_F16_MODIFIERS=0`: as before.
pub(super) fn interp_f16_modifiers() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    on(&ON, "VICTORY_INTERP_F16_MODIFIERS")
}

/// A normalized buffer format's ends load as exactly 1.0 and -1.0, whatever the host's
/// division gives there. From the official #169 (645391bc). `VICTORY_NORM_ENDS=0`: as before.
pub(super) fn norm_ends() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    on(&ON, "VICTORY_NORM_ENDS")
}

/// A mesh program's primitive export holds three 9-bit vertex indices, each with its edge flag
/// above it (bits 9, 19 and 29): the index is the low 9 bits of its 10-bit field. Ours took 10
/// bits, so a corner with its edge flag set named a vertex at 512 or more, and the primitive
/// was culled as naming a vertex past the program's count. From the official #1718
/// (c0228d93). `VICTORY_MESH_EDGE_FLAGS=0`: 10 bits, as before.
pub(super) fn prim_index_mask() -> u32 {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if on(&ON, "VICTORY_MESH_EDGE_FLAGS") {
        0x1FF
    } else {
        0x3FF
    }
}

/// The 16-bit round mode at the instruction being lowered ([`round16`], [`round16_after`]).
pub(super) struct Mode16 {
    entry: Vec<Option<u8>>,
    pub(super) now: Option<u8>,
}

impl Mode16 {
    pub(super) fn new(p: &Program) -> Self {
        Mode16 {
            entry: round16(p),
            now: Some(0),
        }
    }

    /// Lowering enters IR block `b`.
    pub(super) fn enter(&mut self, b: usize) {
        self.now = self.entry[b];
    }

    /// Lowering went past `inst`.
    pub(super) fn after(&mut self, inst: &Inst) {
        self.now = round16_after(inst, self.now);
    }
}

/// A cube sample with explicit gradients takes its level as log2 of the longer gradient in
/// texels: DXIL's `Log` (unary 23) is already base 2, where ours took it for a natural
/// logarithm and multiplied by log2(e), a level 1.44 times too high (blurrier reads); and the
/// `_cl` clamp is applied. From the official 4f57f46f (flagged by amdsh).
/// `VICTORY_CUBE_GRAD_LEVEL=0`: as before.
pub(super) fn cube_grad_level() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    on(&ON, "VICTORY_CUBE_GRAD_LEVEL")
}

/// The 16/64-bit round mode (MODE bits 3:2: 0 nearest even, 3 toward zero) after `inst`, from
/// `mode` before it (`None`: not known). From the official 77053a0c: the title sets toward zero
/// (`s_setreg_imm32_b32 hwreg(HW_REG_MODE, 2, 2), 3`) around its 16-bit interpolation, in some
/// programs for the rest of the program.
pub(super) fn round16_after(inst: &Inst, mode: Option<u8>) -> Option<u8> {
    match (inst.name(), &inst.fields) {
        ("s_round_mode", Fields::Sopp { imm }) => Some((imm >> 2 & 3) as u8),
        (
            "s_setreg_imm32_b32" | "s_setreg_b32",
            Fields::Sop {
                src, imm: Some(h), ..
            },
        ) => {
            let (id, off, size) = (h & 0x3F, h >> 6 & 0x1F, (h >> 11 & 0x1F) + 1);
            if id != 1 || off >= 4 || off + size <= 2 {
                return mode;
            }
            match src.first() {
                Some(&Operand::Literal(x)) if off <= 2 && off + size >= 4 => {
                    Some((x >> (2 - off) & 3) as u8)
                }
                _ => None,
            }
        }
        _ => mode,
    }
}

/// The 16/64-bit round mode each block starts with: nearest even at the program's start, then
/// as the MODE writes on the way set it; `None` where paths that set it differently join (or a
/// write is not a constant). From the official 77053a0c.
pub(super) fn round16(p: &Program) -> Vec<Option<u8>> {
    // None: not reached yet; Some(m): the mode (m None: not known).
    let mut entry: Vec<Option<Option<u8>>> = vec![None; p.blocks.len()];
    entry[0] = Some(Some(0));
    let order = crate::ir::reverse_postorder(&p.blocks);
    loop {
        let mut changed = false;
        for &b in &order {
            let Some(mut m) = entry[b] else { continue };
            for inst in &p.blocks[b].insts {
                m = round16_after(inst, m);
            }
            for s in p.blocks[b].exit.successors() {
                let new = match entry[s] {
                    None => Some(m),
                    Some(old) if old == m => continue,
                    Some(_) => Some(None),
                };
                if entry[s] != new {
                    entry[s] = new;
                    changed = true;
                }
            }
        }
        if !changed {
            return entry.into_iter().map(Option::flatten).collect();
        }
    }
}

impl Lower<'_> {
    /// Scalar operations the title never runs, lowered as the official repository does:
    /// `s_quadmask_b32/b64` (#1443, c3a04e2e). `lo` and `scc` are the values the instruction
    /// writes to its destination's (low) dword and SCC. `None` if `name` is none of them.
    pub(super) fn sop_official(
        &mut self,
        inst: &Inst,
        name: &str,
        lo: Option<Value>,
        scc: Option<Value>,
    ) -> Option<Result<(), Why>> {
        let wide = match name {
            "s_quadmask_b32" => false,
            "s_quadmask_b64" => true,
            _ => return None,
        };
        Some(self.quad_mask_sop(inst, wide, lo, scc))
    }

    /// `s_quadmask_b32` (`wide`: `_b64`) into `lo` (and the next dword) and `scc`.
    fn quad_mask_sop(
        &mut self,
        inst: &Inst,
        wide: bool,
        lo: Option<Value>,
        scc: Option<Value>,
    ) -> Result<(), Why> {
        let Fields::Sop { dst, src, .. } = &inst.fields else {
            return Err("a scalar operation");
        };
        let dword = |l: &mut Self, h: u8| -> Result<V, Why> {
            match src.first() {
                Some(&Operand::Scalar { code, .. }) if code != crate::gfx10::NULL => {
                    let v = inst.read(Reg::S(code + h)).ok_or("an unread operand")?;
                    l.i32_of(v)
                }
                Some(op) if h == 0 => l.operand(inst, op),
                // An inline constant's high dword: its sign.
                Some(&Operand::Const(k)) => {
                    let x = crate::resource::inline(k).ok_or("a special operand")?;
                    Ok(l.s.m.i32(if (x as i32) < 0 { u32::MAX } else { 0 }))
                }
                _ => Ok(l.s.m.i32(0)),
            }
        };
        let c = self.cur;
        let zero = self.s.m.i32(0);
        let mask = if wide {
            let (a, b) = (dword(self, 0)?, dword(self, 1)?);
            let m = self.quad_mask64(a, b);
            let hi = match dst {
                Some(Operand::Scalar { code, .. }) => inst.write(Reg::S(code + 1)),
                _ => None,
            };
            self.set(hi, Val::I32(zero));
            m
        } else {
            let x = dword(self, 0)?;
            self.quad_mask(x)
        };
        self.set(lo, Val::I32(mask));
        let nonzero = self.s.m.cmp(c, pred::NE, mask, zero);
        self.set(scc, Val::Bit(nonzero));
        Ok(())
    }

    /// The high dword of a 32-bit literal sign-extended to 64 bits: the literal of a signed
    /// 64-bit operand (official #447, 7d049431; unsigned and bit operands zero-extend theirs).
    pub(super) fn sext_high(&mut self, x: u32) -> V {
        self.s.m.i32(if (x as i32) < 0 { u32::MAX } else { 0 })
    }

    /// A 32-bit literal sign-extended to an i64 (official #447).
    pub(super) fn sext64(&mut self, x: u32) -> V {
        let i64t = self.s.m.int(64);
        self.s.m.int_const(i64t, i64::from(x as i32))
    }

    /// The mask of a mesh primitive export's vertex index ([`prim_index_mask`]).
    pub(super) fn prim_index_mask(&mut self) -> V {
        self.s.m.i32(prim_index_mask())
    }

    /// `format_load`, its channels packed as d16 data where `d16` (official #169: a
    /// `tbuffer_load_format_d16_*` wrote each channel to a VGPR of its own).
    pub(super) fn format_load_d16(
        &mut self,
        h: V,
        off: V,
        format: u8,
        count: u32,
        d16: bool,
    ) -> Result<Vec<V>, Why> {
        let xs = self.format_load(h, off, format, count)?;
        if d16 {
            self.d16_channels(format, xs)
        } else {
            Ok(xs)
        }
    }

    /// A `v_interp_p2_f16` with op_sel bit 3, omod or clamp ([`Self::interp_f16_modified`]):
    /// its f32 result `f` with omod, then clamp, rounded to 16 bits once and written to the half
    /// op_sel names (official #1696).
    pub(super) fn interp_f16_write(&mut self, inst: &Inst, f: V) -> Result<(), Why> {
        let f = self.interp_f16_output(inst, f);
        let h = self.narrow16(f);
        self.write_half(inst, h, None)
    }

    /// Vector operations the title never runs, lowered as the official repository does:
    /// `v_cvt_off_f32_i4` (#1391), the sums of absolute differences (#162) and the high
    /// dwords of the 24-bit multiplies (#156). `None` if `name` is none of them.
    pub(super) fn valu_official(
        &mut self,
        inst: &Inst,
        name: &str,
        guard: Option<V>,
    ) -> Option<Result<(), Why>> {
        Some(match name {
            "v_cvt_off_f32_i4" => self.cvt_off(inst, guard),
            "v_sad_u8" | "v_sad_hi_u8" | "v_sad_u16" | "v_sad_u32" | "v_msad_u8" => {
                self.sad(inst, name, guard)
            }
            "v_mul_hi_u32_u24" | "v_mul_hi_i32_i24" => self.mul_hi24(inst, name, guard),
            _ => return None,
        })
    }

    /// The low nibble as a signed 4-bit integer, over 16 (official #1391, 78c12677).
    fn cvt_off(&mut self, inst: &Inst, guard: Option<V>) -> Result<(), Why> {
        let c = self.cur;
        let a = self.src(inst, 0)?;
        let k28 = self.s.m.i32(28);
        let a = self.s.m.bin(c, Bin::Shl, a, k28);
        let a = self.s.m.bin(c, Bin::AShr, a, k28);
        let f = self.s.m.cast(c, Cast::SiToFp, a, self.s.t.f32);
        let k = self.s.m.f32(0.0625f32.to_bits());
        let r = self.s.m.bin(c, Bin::Mul, f, k);
        self.write_f(inst, r, guard)
    }

    /// src2 plus the sum of the unsigned differences |a - b| of the operands' bytes, halves or
    /// whole dwords, wrapping at 32 bits; `v_sad_hi_u8` adds the sum shifted to the high half;
    /// `v_msad_u8` leaves out a byte whose byte of src1 is 0 (official #162, 5d6889e8).
    fn sad(&mut self, inst: &Inst, name: &str, guard: Option<V>) -> Result<(), Why> {
        let c = self.cur;
        let (a, b, d) = (self.src(inst, 0)?, self.src(inst, 1)?, self.src(inst, 2)?);
        let (width, parts) = match name {
            "v_sad_u16" => (16, 2),
            "v_sad_u32" => (32, 1),
            _ => (8, 4),
        };
        let zero = self.s.m.i32(0);
        let mut sum = zero;
        for i in 0..parts {
            let (mut x, mut y) = (a, b);
            if width < 32 {
                let (sh, mask) = (self.s.m.i32(width * i), self.s.m.i32((1 << width) - 1));
                x = self.s.m.bin(c, Bin::LShr, x, sh);
                x = self.s.m.bin(c, Bin::And, x, mask);
                y = self.s.m.bin(c, Bin::LShr, y, sh);
                y = self.s.m.bin(c, Bin::And, y, mask);
            }
            let above = self.s.m.cmp(c, pred::IUGT, x, y);
            let hi = self.s.m.select(c, above, x, y);
            let lo = self.s.m.select(c, above, y, x);
            let mut diff = self.s.m.bin(c, Bin::Sub, hi, lo);
            if name == "v_msad_u8" {
                let masked = self.s.m.cmp(c, pred::NE, y, zero);
                diff = self.s.m.select(c, masked, diff, zero);
            }
            sum = self.s.m.bin(c, Bin::Add, sum, diff);
        }
        if name == "v_sad_hi_u8" {
            let k16 = self.s.m.i32(16);
            sum = self.s.m.bin(c, Bin::Shl, sum, k16);
        }
        let r = self.s.m.bin(c, Bin::Add, sum, d);
        self.write_i(inst, r, guard)
    }

    /// The high dword of the 48-bit product of the operands' low 24 bits, zero- or
    /// sign-extended (official #156, 344704e0).
    fn mul_hi24(&mut self, inst: &Inst, name: &str, guard: Option<V>) -> Result<(), Why> {
        let c = self.cur;
        let (a, b) = (self.src(inst, 0)?, self.src(inst, 1)?);
        let signed = name == "v_mul_hi_i32_i24";
        let (sh, op) = (self.s.m.i32(8), if signed { Bin::AShr } else { Bin::LShr });
        let a = self.s.m.bin(c, Bin::Shl, a, sh);
        let a = self.s.m.bin(c, op, a, sh);
        let b = self.s.m.bin(c, Bin::Shl, b, sh);
        let b = self.s.m.bin(c, op, b, sh);
        let r = self.mul_hi(a, b, signed);
        self.write_i(inst, r, guard)
    }

    /// `s_quadmask_b32`: a bit per quad of `pixels` (four bits), whether any of its pixels is
    /// set, packed into the low byte (official #1443, c3a04e2e).
    fn quad_mask(&mut self, pixels: V) -> V {
        let c = self.cur;
        let one = self.s.m.i32(1);
        let shifted = self.s.m.bin(c, Bin::LShr, pixels, one);
        let mut mask = self.s.m.bin(c, Bin::Or, pixels, shifted);
        // Fold each nibble into its bottom bit, then gather those bits into the low byte in
        // three stages without a 64-bit operation.
        for (distance, keep) in [
            (2, 0x1111_1111),
            (3, 0x0303_0303),
            (6, 0x000f_000f),
            (12, 0x0000_00ff),
        ] {
            let distance = self.s.m.i32(distance);
            let shifted = self.s.m.bin(c, Bin::LShr, mask, distance);
            let combined = self.s.m.bin(c, Bin::Or, mask, shifted);
            let keep = self.s.m.i32(keep);
            mask = self.s.m.bin(c, Bin::And, combined, keep);
        }
        mask
    }

    /// `s_quadmask_b64` of the pair `lo`, `hi`: the result's low dword (its high one is 0;
    /// official #1443).
    fn quad_mask64(&mut self, lo: V, hi: V) -> V {
        let c = self.cur;
        let low = self.quad_mask(lo);
        let high = self.quad_mask(hi);
        let k8 = self.s.m.i32(8);
        let high = self.s.m.bin(c, Bin::Shl, high, k8);
        self.s.m.bin(c, Bin::Or, low, high)
    }

    /// Whether a `v_interp_p2_f16` has a modifier ours left out: op_sel bit 3 (the high half),
    /// omod or clamp (official #1696; [`interp_f16_modifiers`]). Without one, the result is
    /// written as before.
    pub(super) fn interp_f16_modified(&self, inst: &Inst) -> bool {
        let Fields::Valu(v) = &inst.fields else {
            return false;
        };
        interp_f16_modifiers() && (v.opsel & 8 != 0 || v.omod != 0 || v.clamp)
    }

    /// A `v_interp_p2_f16` result `f` (f32) with the instruction's omod, then clamp, applied
    /// before it is rounded to 16 bits once (official #1696).
    pub(super) fn interp_f16_output(&mut self, inst: &Inst, f: V) -> V {
        let Fields::Valu(v) = &inst.fields else {
            return f;
        };
        let mut f = f;
        if v.omod != 0 {
            let factor = [1.0f32, 2.0, 4.0, 0.5][usize::from(v.omod & 3)];
            let k = self.s.m.f32(factor.to_bits());
            f = self.s.m.bin(self.cur, Bin::Mul, f, k);
        }
        if v.clamp {
            f = self.unary(7, f);
        }
        f
    }

    /// A normalized `w`-bit field's quotient `q` with its ends exact: 1.0 at the largest
    /// value, and for a signed field `s`, -1.0 at its lowest two (official #169, 645391bc:
    /// a host may divide through a reciprocal and land just below 1.0, as RDNA 4 loaded
    /// 16_16_UNORM's 65535 as the half 0x3BFF).
    pub(super) fn norm_exact(&mut self, q: V, field: V, w: u32, signed: bool) -> V {
        if !norm_ends() {
            return q;
        }
        let c = self.cur;
        let one = self.s.m.f32(1.0f32.to_bits());
        if !signed {
            let top = self.s.m.i32(((1u64 << w) - 1) as u32);
            let at_top = self.s.m.cmp(c, pred::EQ, field, top);
            return self.s.m.select(c, at_top, one, q);
        }
        let top = (1u32 << (w - 1)) - 1;
        let high = self.s.m.i32(top);
        let at_top = self.s.m.cmp(c, pred::SGE, field, high);
        let q = self.s.m.select(c, at_top, one, q);
        let low = self.s.m.i32(top.wrapping_neg());
        let minus_one = self.s.m.f32((-1.0f32).to_bits());
        let at_bottom = self.s.m.cmp(c, pred::SLE, field, low);
        self.s.m.select(c, at_bottom, minus_one, q)
    }

    /// Channels of buffer format `format`, as `format_load` gives them, as d16 data: 16 bits
    /// each, two per dword, low half first (an odd last high half 0). Integer formats keep
    /// their low 16 bits; the others become f16. For `tbuffer_load_format_d16_*` (official
    /// #169, c430b1de), which wrote each channel to a VGPR of its own.
    pub(super) fn d16_channels(&mut self, format: u8, channels: Vec<V>) -> Result<Vec<V>, Why> {
        let (_, numeric) = super::buffer_format(format).ok_or("a buffer format")?;
        let c = self.cur;
        let mut halves = Vec::new();
        for x in channels {
            halves.push(
                if matches!(numeric, super::Numeric::Uint | super::Numeric::Sint) {
                    let m = self.s.m.i32(0xFFFF);
                    self.s.m.bin(c, Bin::And, x, m)
                } else {
                    let (f, i32t) = (self.as_float(x), self.s.t.i32);
                    self.s
                        .op(c, "legacyF32ToF16", super::Attr::ReadNone, i32t, 130, &[f])
                },
            );
        }
        let mut out = Vec::new();
        for pair in halves.chunks(2) {
            out.push(match *pair {
                [lo, hi] => {
                    let k16 = self.s.m.i32(16);
                    let hi = self.s.m.bin(c, Bin::Shl, hi, k16);
                    self.s.m.bin(c, Bin::Or, lo, hi)
                }
                [lo] => lo,
                _ => unreachable!("chunks of one or two"),
            });
        }
        Ok(out)
    }

    /// Whether a `v_cndmask_b32` mask (`op`, the register's value `mask`) that the structurisation
    /// does not hold among its lane masks is one taken here (official #1221, 67c4f531): an
    /// inline constant, or a mask it keeps among its other masks (a uniform select, say),
    /// except in a compute program with the console's lanes, which reads it as the number it is.
    pub(super) fn cndmask_more(&self, op: Option<&Operand>, mask: Option<Value>) -> bool {
        match op {
            Some(&Operand::Const(k)) => crate::resource::inline(k).is_some(),
            Some(Operand::Scalar { .. }) => {
                (self.mask_data != MaskData::Console || !self.console_lanes)
                    && mask.is_some_and(|m| self.x.st.other.contains_key(&m))
            }
            _ => false,
        }
    }

    /// The lane condition of such a mask ([`Self::cndmask_more`]).
    pub(super) fn cndmask_cond(
        &mut self,
        op: Option<&Operand>,
        mask: Option<Value>,
        c: usize,
    ) -> Result<V, Why> {
        if let Some(&Operand::Const(k)) = op {
            let x = crate::resource::inline(k).ok_or("a select on an untracked mask")?;
            return match x {
                0 => Ok(self.s.m.bool(false)),
                u32::MAX => Ok(self.s.m.bool(true)),
                // Another constant: this lane's bit of it, where lanes are the console's (an
                // inline constant sign-extends to the high dword of a wave64 mask) for other
                // reasons already.
                _ if self.mask_data == MaskData::Console && self.console_lanes && !self.local => {
                    self.cross_lane(false)?;
                    let lo = self.s.m.i32(x);
                    let wide = self.x.native.wave64;
                    let hi = wide.then(|| self.s.m.i32(if (x as i32) < 0 { u32::MAX } else { 0 }));
                    Ok(self.lane_bit(c, lo, hi))
                }
                _ => Err("a constant lane mask"),
            };
        }
        let m = mask.and_then(|m| self.x.st.other.get(&m).copied());
        let m = m.ok_or("a select on an untracked mask")?;
        self.boolean(m, c)
    }

    /// `f` as an f16 in the low bits, as a 16-bit result of the console's: with
    /// `VICTORY_F16_NEAREST=1`, rounded as the program's MODE says at this point (official
    /// 77053a0c): toward zero where it set that, else to nearest even (also where it is not
    /// known); without it, the host's conversion as before (toward zero on NVIDIA, which the
    /// title's toward-zero sections get right and the others one place low).
    pub(super) fn narrow16(&mut self, f: V) -> V {
        if !super::f16_nearest_on() {
            let i32t = self.s.t.i32;
            return self
                .s
                .op(self.cur, "legacyF32ToF16", Attr::ReadNone, i32t, 130, &[f]);
        }
        if self.mode16.now == Some(3) {
            self.f16_toward_zero(f)
        } else {
            self.f16_nearest(f)
        }
    }

    /// `f` times the output multiplier `k` (1 `mul:2`, 2 `mul:4`, 3 `div:2`), as
    /// [`omod_zero`] says.
    pub(super) fn omod(&mut self, f: V, k: u8) -> V {
        let c = self.cur;
        let factor = [1.0f32, 2.0, 4.0, 0.5][usize::from(k & 3)];
        let m = self.s.m.f32(factor.to_bits());
        if !omod_zero() {
            return self.s.m.bin(c, Bin::Mul, f, m);
        }
        let (exp, sign, zero) = (
            self.s.m.i32(0x7F80_0000),
            self.s.m.i32(0x8000_0000),
            self.s.m.i32(0),
        );
        // The operand's denormal as a zero of its sign, on the bits.
        let x = self.as_int(f);
        let e = self.s.m.bin(c, Bin::And, x, exp);
        let denormal = self.s.m.cmp(c, pred::EQ, e, zero);
        let s = self.s.m.bin(c, Bin::And, x, sign);
        let x = self.s.m.select(c, denormal, s, x);
        let f = self.as_float(x);
        // A zero or denormal product: +0.
        let p = self.s.m.bin(c, Bin::Mul, f, m);
        let x = self.as_int(p);
        let e = self.s.m.bin(c, Bin::And, x, exp);
        let tiny = self.s.m.cmp(c, pred::EQ, e, zero);
        let x = self.s.m.select(c, tiny, zero, x);
        self.as_float(x)
    }

    /// The level of a cube sample with explicit gradients, from the longer gradient in texels
    /// of a face (`texels`): log2 of it (DXIL's `Log`), at least the `_cl` clamp where there is
    /// one (official 4f57f46f; see [`cube_grad_level`]). With the switch off, as before.
    pub(super) fn cube_level(&mut self, texels: V, clamp: Option<V>) -> V {
        let c = self.cur;
        let l = self.unary(23, texels);
        if !cube_grad_level() {
            let per_ln2 = self.s.m.f32(std::f32::consts::LOG2_E.to_bits());
            return self.s.m.bin(c, Bin::Mul, l, per_ln2);
        }
        match clamp {
            Some(cl) => self.binary(35, l, cl),
            None => l,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Parts, lower};
    use crate::native::{ComputeIo, HwStage, NativeProgram, StageIo};
    use crate::{io, ir, resource, structure};

    const S_ENDPGM: u32 = 0xBF81_0000;

    /// A synthetic one-thread compute program, lowered.
    fn lowers(words: &[u32], rsrc2: u32) -> Result<(), crate::ShaderError> {
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let native = NativeProgram {
            stage: HwStage::Cs,
            wave64: false,
            vgprs: 256,
            layout: vec![],
            used: vec![],
            io: StageIo::Cs(ComputeIo {
                rsrc2,
                threads: [1, 1, 1],
            }),
            code,
        };
        let p = ir::build(&native.code, false, 256)?;
        let st = structure::structure(&p)?;
        let res = resource::track(&p, &native.layout, native.stage, Some(&st))?;
        let io = io::raise(&p, &native)?;
        lower(&Parts {
            native: &native,
            p: &p,
            st: &st,
            io: &io,
            res: &res,
            inputs: vec![],
            outputs: vec![],
            prims: vec![],
            mesh_stride: 0,
        })
        .map(|_| ())
    }

    /// s0, s1 and v1, v2, v3 set, `body`, then v0 stored to LDS at address v0 (so the result
    /// is used).
    fn program(body: &[u32]) -> Vec<u32> {
        let mut words = vec![
            0xBE80_0385, // s_mov_b32 s0, 5
            0xBE81_0383, // s_mov_b32 s1, 3
            0x7E02_0281, // v_mov_b32 v1, 1
            0x7E04_0282, // v_mov_b32 v2, 2
            0x7E06_0283, // v_mov_b32 v3, 3
        ];
        words.extend_from_slice(body);
        words.extend([0xD834_0000, 0x0000_0000, S_ENDPGM]); // ds_write_b32 v0, v0
        words
    }

    /// 512 bytes of LDS.
    const RSRC2: u32 = 1 << 15;

    #[test]
    fn sums_of_absolute_differences_lower() {
        // VOP3 `op v0, v1, v2, v3`: v_sad_u8, v_sad_hi_u8, v_sad_u16, v_sad_u32 and v_msad_u8
        // (official #162).
        for op in [0x15Au32, 0x15B, 0x15C, 0x15D, 0x171] {
            let words = program(&[0xD400_0000 | op << 16, 0x101 | 0x102 << 9 | 0x103 << 18]);
            lowers(&words, RSRC2).unwrap_or_else(|e| panic!("VOP3 {op:#x}: {e}"));
        }
    }

    #[test]
    fn the_24_bit_high_multiplies_and_cvt_off_lower() {
        // VOP2 v_mul_hi_i32_i24 and v_mul_hi_u32_u24 v0, v1, v2 (official #156), and VOP1
        // v_cvt_off_f32_i4 v0, v1 (official #1391).
        for word in [
            0x0A << 25 | 2 << 9 | 0x101,
            0x0C << 25 | 2 << 9 | 0x101,
            0x7E00_1D01,
        ] {
            lowers(&program(&[word]), RSRC2).unwrap_or_else(|e| panic!("{word:#010x}: {e}"));
        }
    }

    #[test]
    fn scalar_quad_masks_lower() {
        // s_quadmask_b32 s16, s0 and s_quadmask_b64 s[16:17], s[0:1] (official #1443), then
        // v_mov_b32 v0, s16.
        for word in [0xBE90_2C00u32, 0xBE90_2D00] {
            let words = program(&[word, 0x7E00_0210]);
            lowers(&words, RSRC2).unwrap_or_else(|e| panic!("{word:#010x}: {e}"));
        }
    }

    /// Official #1221 (67c4f531): a select on a uniform mask (`s_cselect_b32 s0, -1, 0`) and on
    /// an inline constant mask (`-1`), which ours refused ("a select on an untracked mask").
    #[test]
    fn v_cndmask_accepts_uniform_and_constant_masks() {
        let words = [
            0x7E00_0500u32, // v_readfirstlane_b32 s0, v0
            0xBF06_8000,    // s_cmp_eq_u32 s0, 0
            0x8500_80C1,    // s_cselect_b32 s0, -1, 0
            0xD501_0001,
            0x0001_0080, // v_cndmask_b32 v1, 0, 1, s0
            0xD501_0002,
            0x0201_02C1, // v_cndmask_b32 v2, 1, v1, -1
            0xD834_0000,
            0x0000_0200, // ds_write_b32 v0, v2
            S_ENDPGM,
        ];
        lowers(&words, RSRC2).unwrap();
    }
}
