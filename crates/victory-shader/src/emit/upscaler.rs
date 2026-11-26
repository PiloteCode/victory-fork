//! Forms only the FidelityFX upscaler's compiler makes (its programs come in `ACCC` parts, see
//! `native::fspx`), lowered as avatarmanz's translator does (66afb2b4, 389db9efb); a child
//! module of the lowering so it shares the emitter's state. No program of the title's shader
//! archive runs them: none of their translations changes with these.

use super::super::wave::Shape;
use super::{Attr, Bin, Cast, EXEC, Fields, Inst, Lower, Operand, Reg, V, Val, Value, Why};

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

    /// Whether the mask operation that sets the SCC value `scc` gives no lane outside EXEC a
    /// bit (it writes EXEC, or a mask the lanes' paths know as one within EXEC): its SCC as a
    /// number is then "some lane of the wave is left" (`scc_some`), as the upscaler's programs
    /// read it (a loop that waits for other groups until every lane saw its flag, or after
    /// enough rounds; 389db9efb).
    pub(super) fn within_exec(&self, scc: Value) -> bool {
        let Some(&b) = self.def_block.get(&scc) else {
            return false;
        };
        let by = self.x.p.blocks[b].insts.iter().find(|i| i.write(Reg::Scc) == Some(scc));
        by.is_some_and(|i| {
            i.write(Reg::S(EXEC)).is_some()
                || matches!(&i.fields, Fields::Sop { dst: Some(Operand::Scalar { code, .. }), .. }
                    if i.write(Reg::S(*code)).is_some_and(|lo| self.x.st.active.contains(&lo)))
        })
    }

    /// For a `v_readlane_b32` (instruction `i` of block `b`) of one lane of a VGPR a
    /// whole-wave section earlier in the block left, where that lane holds a value of the wave
    /// ([`super::super::wave::Section::reads`]): the scalar it writes and that value's shape.
    pub(super) fn section_read(&self, inst: &Inst, b: usize, i: usize) -> Option<(Value, Shape)> {
        if inst.name() != "v_readlane_b32" {
            return None;
        }
        let &(_, w) = inst.writes.first()?;
        let sections = self.sections.get(&b)?;
        sections.values().filter(|s| s.end < i).find_map(|s| s.reads.get(&w).copied()).map(|sh| (w, sh))
    }

    /// [`Self::section_read`] lowered: the value over the wave's lanes (in the paired form,
    /// both lanes of every thread, as [`Self::paired_section_inst`] makes it).
    pub(super) fn section_read_value(&mut self, w: Value, shape: Shape) -> Result<(), Why> {
        if self.twin.is_none() {
            let x = self.shape(shape, false)?;
            self.vals.insert(w, Val::I32(x));
            return Ok(());
        }
        let made = match shape {
            Shape::Active(..) | Shape::Prefix(..) | Shape::Sum64 { .. } => self.paired_shape(shape),
            _ => self.both(|l| l.shape(shape, false)),
        };
        match made {
            Ok([lo, hi]) => self.put_lanes(w, Val::I32(lo), Val::I32(hi)),
            Err(why) => self.put_lanes(w, Val::Missing(why), Val::Missing(why)),
        }
        Ok(())
    }

    /// The operand `src_scc` of a vector instruction: SCC as 0 or 1.
    pub(super) fn src_scc(&mut self, inst: &Inst) -> Result<V, Why> {
        let bit = self.scc_in(inst)?;
        Ok(self.s.m.cast(self.cur, Cast::ZExt, bit, self.s.t.i32))
    }
}
