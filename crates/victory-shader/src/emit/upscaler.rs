//! Forms only the FidelityFX upscaler's compiler makes (its programs come in `ACCC` parts, see
//! `native::fspx`), lowered as avatarmanz's translator does (66afb2b4, 389db9efb); a child
//! module of the lowering so it shares the emitter's state. No program of the title's shader
//! archive runs them: none of their translations changes with these.

use super::{Attr, Bin, Cast, Fields, Inst, Lower, Operand, Reg, V, Why};

/// `dx.op.dot4AddPacked.i32` (SM 6.4): an accumulator plus the dot product of the four signed
/// bytes of two dwords, wrapping.
const DOT4_ADD_I8_PACKED: u32 = 163;
/// The operand code of `src_scc`.
pub(super) const SRC_SCC: u8 = 253;

impl Lower<'_> {
    /// The byte dot products (`v_dot4_i32_i8`, `v_dot4c_i32_i8`); `None` for any other
    /// instruction.
    pub(super) fn valu_upscaler(&mut self, inst: &Inst, name: &str, guard: Option<V>) -> Option<Result<(), Why>> {
        Some(match name {
            "v_dot4c_i32_i8" => self.dot4c(inst, guard),
            "v_dot4_i32_i8" => self.dot4(inst, guard),
            _ => return None,
        })
    }

    /// `v_dot4c_i32_i8` (VOP2): the destination accumulates the dot product of the sources.
    fn dot4c(&mut self, inst: &Inst, guard: Option<V>) -> Result<(), Why> {
        let Fields::Valu(v) = &inst.fields else {
            unreachable!()
        };
        let (a, b) = (self.src(inst, 0)?, self.src(inst, 1)?);
        let Some(Operand::Vector { reg, .. }) = v.dst else {
            return Err("an accumulator");
        };
        let acc = self.i32_of(inst.read(Reg::V(reg)).ok_or("an accumulator")?)?;
        let r = self.dot4_add(acc, a, b);
        self.write_i(inst, r, guard)
    }

    /// `v_dot4_i32_i8` (VOP3P): `op_sel` picks the half each multiplicand's low half reads,
    /// `op_sel_hi` its high half's (both set, the identity, as assemblers write it); the
    /// accumulator is read whole (as AMD's compilers fold constants into it).
    fn dot4(&mut self, inst: &Inst, guard: Option<V>) -> Result<(), Why> {
        let Fields::Valu(v) = &inst.fields else {
            unreachable!()
        };
        if v.clamp || v.neg_hi != 0 {
            return Err("a saturating or negated dot product");
        }
        let (opsel, opsel_hi) = (v.opsel, v.opsel_hi);
        let c = self.cur;
        let mut xs = Vec::new();
        for k in 0..2 {
            let x = self.src(inst, k)?;
            let (lo, hi) = (opsel >> k & 1 != 0, opsel_hi >> k & 1 != 0);
            if (lo, hi) == (false, true) {
                xs.push(x);
                continue;
            }
            let (k16, low) = (self.s.m.i32(16), self.s.m.i32(0xFFFF));
            let top = self.s.m.bin(c, Bin::LShr, x, k16);
            let bottom = self.s.m.bin(c, Bin::And, x, low);
            let high = self.s.m.bin(c, Bin::Shl, if hi { top } else { bottom }, k16);
            xs.push(self.s.m.bin(c, Bin::Or, if lo { top } else { bottom }, high));
        }
        let acc = self.src(inst, 2)?;
        let r = self.dot4_add(acc, xs[0], xs[1]);
        self.write_i(inst, r, guard)
    }

    fn dot4_add(&mut self, acc: V, a: V, b: V) -> V {
        let i32t = self.s.t.i32;
        self.s.op(self.cur, "dot4AddPacked.i32", Attr::ReadNone, i32t, DOT4_ADD_I8_PACKED, &[acc, a, b])
    }

    /// The operand `src_scc` of a vector instruction: SCC as 0 or 1.
    pub(super) fn src_scc(&mut self, inst: &Inst) -> Result<V, Why> {
        let bit = self.scc_in(inst)?;
        Ok(self.s.m.cast(self.cur, Cast::ZExt, bit, self.s.t.i32))
    }
}
