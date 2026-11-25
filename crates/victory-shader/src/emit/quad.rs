//! Reads of the other lanes of a lane's quad (lanes 4n..4n+3): a pixel program's quad-local lane
//! reads through the host's quad operations, and the derivatives a compute program's sample
//! takes from its quad of lanes.
//!
//! A pixel program's quad is a 2x2 block of pixels, some of them helper lanes at a triangle's
//! edge (and lanes a discard ended). The console runs the code that feeds derivatives in
//! whole-quad mode, so a `ds_swizzle` or DPP `quad_perm` there reads the helper lanes' values
//! as well as the others'. On the host only the quad operations are defined to reach helper
//! lanes: SM 6.6 leaves them out of wave operations, whose reads of them are undefined (SM 6.7
//! lets a program ask for them with `WaveOpsIncludeHelperLanes`). After Belobug's fork
//! (03629f3f, its AMD host read 0 from helper lanes through wave reads: a derivative taken by
//! hand at a triangle's edge was the whole value, and the texture read using it took its finest
//! level, which showed as lines along every triangle on car paint, glass and skin).
//!
//! A compute program's implicit-level sample (`image_sample`, `image_sample_b`) takes its
//! level from the quad of lanes it runs in on the console, as a pixel program's does; DXIL
//! before validator 1.8 has no implicit derivatives outside pixel programs. The quad's
//! differences become explicit gradients (lane 1 minus lane 0 across, lane 2 minus lane 0
//! down), read lane by lane in the console's lane order, after Belobug's fork (e510ba7b,
//! c78056c9). Before, such a sample read level 0: the finest, whatever the quad's footprint.

use super::{Attr, Bin, Lower, V, flags};

/// The quad operations of `dx.op.quadOp` (opcode 123), by the lane they read: across x (lane
/// ^ 1), across y (lane ^ 2), across the diagonal (lane ^ 3).
const QUAD_OP: [u8; 3] = [0, 1, 2];

/// Whether a sample instruction takes its level from implicit derivatives (the ones the
/// lowering of samples gives `sample` or `sampleBias`).
pub(in crate::emit) fn implicit_level(name: &str) -> bool {
    matches!(
        name,
        "image_sample"
            | "image_sample_o"
            | "image_sample_cl"
            | "image_sample_b"
            | "image_sample_b_cl"
    )
}

/// The lane of a quad (0..3) that place `k` of it reads with a `ds_swizzle` of `offset` that
/// stays in the quad ([`Lower::ds`]'s test): its quad pattern (bit 15), or its and/or/xor
/// masks, which then change bits 0 and 1 alone.
pub(super) fn swizzle_picks(offset: u32) -> [u32; 4] {
    if offset & 0x8000 != 0 {
        return quad_perm_picks(offset & 0xFF);
    }
    let (and, or, xor) = (offset & 3, offset >> 5 & 3, offset >> 10 & 3);
    [0, 1, 2, 3].map(|k| (k & and | or) ^ xor)
}

/// The lane of a quad that place `k` of it reads with DPP `quad_perm` control `ctrl`.
pub(super) fn quad_perm_picks(ctrl: u32) -> [u32; 4] {
    [0, 1, 2, 3].map(|k| ctrl >> (2 * k) & 3)
}

impl<'a> Lower<'a> {
    /// Whether a quad-local lane read of this program goes through quad operations
    /// ([`super::super::quad_reads`]): a pixel program's.
    pub(super) fn quad_ops(&self) -> bool {
        self.s.pixel && super::super::quad_reads()
    }

    /// `x` of the lane of this lane's quad that `picks` names for this lane's place in it (place
    /// k reads lane `picks[k]`), through quad operations: one where every place reads the same
    /// lane (`QuadReadLaneAt`) or the lane across x, y or the diagonal (`QuadReadAcross*`), else
    /// the lanes it reads and a choice by place (the place is the host lane's low two bits, as
    /// the wave reads this replaces took it).
    pub(super) fn quad_read(&mut self, x: V, picks: [u32; 4]) -> V {
        self.s.flags |= flags::WAVE_OPS;
        let c = self.cur;
        let i32t = self.s.t.i32;
        if picks.iter().all(|&p| p == picks[0]) {
            let k = self.s.m.i32(picks[0]);
            return self
                .s
                .op(c, "quadReadLaneAt.i32", Attr::NoUnwind, i32t, 122, &[x, k]);
        }
        let across = picks[0];
        if (0..4).all(|k| picks[k as usize] == k ^ across) {
            // (across is not 0 here: the picks differ)
            let op = self.s.m.i8(QUAD_OP[across as usize - 1]);
            return self
                .s
                .op(c, "quadOp.i32", Attr::NoUnwind, i32t, 123, &[x, op]);
        }
        let mut read = [None; 4];
        for &p in &picks {
            if read[p as usize].is_none() {
                let k = self.s.m.i32(p);
                read[p as usize] =
                    Some(
                        self.s
                            .op(c, "quadReadLaneAt.i32", Attr::NoUnwind, i32t, 122, &[x, k]),
                    );
            }
        }
        let at = |k: usize| read[picks[k] as usize].expect("read above");
        let lane = self.lane_index();
        let three = self.s.m.i32(3);
        let place = self.s.m.bin(c, Bin::And, lane, three);
        let mut r = at(3);
        for k in (0..3).rev() {
            if picks[k] == picks[3] {
                continue;
            }
            let kk = self.s.m.i32(k as u32);
            let here = self.s.m.cmp(c, super::pred::EQ, place, kk);
            r = self.s.m.select(c, here, at(k), r);
        }
        r
    }

    /// Whether this program's implicit-level samples take their gradients from the quad of
    /// lanes (a compute program's, [`super::super::compute_derivatives`]); with the switch off,
    /// or in a vertex or mesh program, such a sample reads level 0 (a bias: that level).
    pub(super) fn quad_derivatives(&self) -> bool {
        self.s.threads.is_some() && self.s.mesh.is_none() && super::super::compute_derivatives()
    }

    /// Coarse derivatives of `coords` across this lane's quad of console lanes (lane 1 minus
    /// lane 0 across, lane 2 minus lane 0 down), as DXIL gradients: x of up to three
    /// coordinates, then y; each times `scale` if given (2^bias: the same level for isotropic
    /// filtering). Host lane n is console lane n here (console lanes; in the paired form the
    /// lane being lowered holds its own coordinates, and its quad is in its half).
    pub(super) fn quad_gradients(&mut self, coords: &[V], scale: Option<V>) -> Vec<V> {
        let c = self.cur;
        let lane = self.lane_index();
        let top = self.s.m.i32(!3);
        let base = self.s.m.bin(c, Bin::And, lane, top);
        let lanes: Vec<V> = (1..3u32)
            .map(|k| {
                let k = self.s.m.i32(k);
                self.s.m.bin(c, Bin::Or, base, k)
            })
            .collect();
        let fu = self.s.m.undef(self.s.t.f32);
        let mut g = vec![fu; 6];
        for (k, &x) in coords.iter().enumerate().take(3) {
            let xi = self.as_int(x);
            let at = |l: &mut Self, lane: V| {
                let r = l.read_lane(xi, lane);
                l.as_float(r)
            };
            let origin = at(self, base);
            for (d, &lane) in lanes.iter().enumerate() {
                let there = at(self, lane);
                let mut diff = self.s.m.bin(c, Bin::Sub, there, origin);
                if let Some(s) = scale {
                    diff = self.s.m.bin(c, Bin::Mul, diff, s);
                }
                g[3 * d + k] = diff;
            }
        }
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swizzle_and_dpp_quad_patterns_name_the_lanes_read() {
        // quad_perm [1, 0, 3, 2]: across x
        assert_eq!(quad_perm_picks(0b10_11_00_01), [1, 0, 3, 2]);
        // broadcast lane 2
        assert_eq!(quad_perm_picks(0xAA), [2, 2, 2, 2]);
        // ds_swizzle quad mode, the same pattern
        assert_eq!(swizzle_picks(0x8000 | 0b10_11_00_01), [1, 0, 3, 2]);
        // and/or/xor masks within the quad: and 0x1F, xor 2 (lane ^ 2)
        assert_eq!(swizzle_picks(0x1F | 2 << 10), [2, 3, 0, 1]);
        // and 0x1C (the quad's first lane), or 1: lane 1 of the quad for all
        assert_eq!(swizzle_picks(0x1C | 1 << 5), [1, 1, 1, 1]);
    }

    #[test]
    fn implicit_levels_are_the_plain_and_biased_samples() {
        assert!(implicit_level("image_sample"));
        assert!(implicit_level("image_sample_b_cl"));
        assert!(!implicit_level("image_sample_l"));
        assert!(!implicit_level("image_sample_lz"));
        assert!(!implicit_level("image_sample_d"));
        assert!(!implicit_level("image_sample_c"));
        assert!(!implicit_level("image_gather4"));
    }
}
