//! The native program of an `ACCC` part: a container carries one in place of
//! `DXIL` when another compiler made the GFX10 code (the title's FidelityFX
//! upscaler, whose compute programs come in its DLL, not in the shader
//! archive). Read as avatarmanz's translator reads it (66afb2b4). Layout as
//! observed (offsets from "FSPX"):
//!
//! ```text
//! +0x00 "FSPX" u32 version (1), u32 entry count, u32 size; entries of 16 B
//!       from +0x10: u32 kind, u32 offset, u32 size, u32 0
//! kind 0  pipeline registers, 0x1c B: +0x0c u16 x3 thread-group size,
//!         +0x12 u16 flags (bit 4 wave32), +0x14 COMPUTE_PGM_RSRC1,
//!         +0x18 COMPUTE_PGM_RSRC2
//! kind 1  pipeline metadata (a hash); kind 6 the GFX10 code
//! kind 7  program metadata: u32 0x403, u32 size - 0x14, u32 code dwords,
//!         u64 0, u8 0x40, u8 first user-data dword, u8 count of 0x41
//!         records, u8 0; then 32-byte records from +0x18, each a u32 tag:
//!   0x41  a resource the code reads: u8 resource type (as FSSM's used
//!         entries), u8 root type, u16 0, u32 table offset (dwords), u32
//!         register, u32 view dimension (1 buffer, 4 2D, 0 root view),
//!         u8 descriptor dwords, u8 user-data dword, u8 root slot, u8 0,
//!         u32 -1, u32 0
//!   0x42  a view declaration: u32 0, u8 1, u8 class (0 SRV, 1 UAV),
//!         u8 resource kind (DXIL's), u8 0, u32 register
//!   0x43  the shader hash (16 B); the last record
//! ```
//!
//! There is no stride (no used entry of the title's programs is structured)
//! and no stage I/O beyond the compute dispatch settings. Each root slot a
//! resource names is a root parameter, at the user-data dword its first
//! resource gives.

use super::{
    ComputeIo, HwStage, MAX_XDAT_ENTRIES, NativeProgram, ResourceType, RootParam, RootType,
    StageIo, UsedResource, bytes, le32,
};
use crate::ShaderError::{self, Malformed};
use crate::dxbc::{u16_at, u32_at};

pub(super) const MAGIC: &[u8; 4] = b"FSPX";
const VERSION: u32 = 1;
const HEADER: usize = 0x10;
const ENTRY: usize = 16;
const KIND_PACKET: u32 = 0;
const KIND_CODE: u32 = 6;
const KIND_METADATA: u32 = 7;
const PACKET_THREADS: usize = 0x0c;
const PACKET_FLAGS: usize = 0x12;
const PACKET_WAVE32: u16 = 1 << 4;
const PACKET_RSRC1: usize = 0x14;
const PACKET_RSRC2: usize = 0x18;
const META_CODE_DWORDS: usize = 8;
const RECORDS: usize = 0x18;
const RECORD: usize = 32;
const TAG_RESOURCE: u8 = 0x41;
const TAG_VIEW: u8 = 0x42;
const TAG_HASH: u8 = 0x43;

/// The compute program of an `ACCC` part's data (which starts with
/// [`MAGIC`]).
pub(super) fn program(part: &[u8]) -> Result<NativeProgram, ShaderError> {
    if u32_at(part, 4)? != VERSION {
        return Err(Malformed("unknown FSPX version"));
    }
    let count = u32_at(part, 8)? as usize;
    if count > MAX_XDAT_ENTRIES {
        return Err(Malformed("implausible FSPX entry count"));
    }
    let entry = |kind: u32| -> Result<&[u8], ShaderError> {
        for i in 0..count {
            let at = HEADER + ENTRY * i;
            if u32_at(part, at)? == kind {
                let (offset, size) = (u32_at(part, at + 4)?, u32_at(part, at + 8)?);
                return bytes(part, offset.into(), size.into())
                    .ok_or(Malformed("FSPX entry out of bounds"));
            }
        }
        Err(Malformed("FSPX entry missing"))
    };
    let packet = entry(KIND_PACKET)?;
    let code = entry(KIND_CODE)?;
    let meta = entry(KIND_METADATA)?;
    if u32_at(meta, META_CODE_DWORDS)? as usize * 4 != code.len() {
        return Err(Malformed("FSPX code size disagrees"));
    }
    let wave64 = u16_at(packet, PACKET_FLAGS)? & PACKET_WAVE32 == 0;
    // VGPRs in granules of 4 (wave64) or 8 (wave32).
    let granules = (u32_at(packet, PACKET_RSRC1)? & 0x3F) + 1;
    let vgprs = (granules * if wave64 { 4 } else { 8 }).min(256) as u16;
    let threads = |i: usize| u16_at(packet, PACKET_THREADS + 2 * i).map(u32::from);
    let mut layout: Vec<RootParam> = Vec::new();
    let mut used = Vec::new();
    for e in meta.get(RECORDS..).unwrap_or(&[]).as_chunks::<RECORD>().0 {
        match e[0] {
            TAG_RESOURCE => {}
            TAG_VIEW => continue,
            TAG_HASH => break,
            _ => return Err(Malformed("unknown FSPX metadata record")),
        }
        let root = RootType::from_raw(e[5].into());
        let u = UsedResource {
            kind: match e[4] {
                0 => ResourceType::Srv,
                1 => ResourceType::Sampler,
                2 => ResourceType::Cbv,
                3 => ResourceType::Uav,
                n => ResourceType::Other(n),
            },
            root,
            table_offset_dwords: le32(e, 8),
            lower_bound: le32(e, 12),
            stride: 0,
            dwords: e[20],
            user_data: e[21],
            slot: e[22],
        };
        let sgprs = match root {
            RootType::Table => 1,
            RootType::Descriptor => 2,
            // The code holds its words; no user data.
            RootType::ImmediateSampler => 0,
            _ => return Err(Malformed("an FSPX resource of an unknown root type")),
        };
        if sgprs > 0 && !layout.iter().any(|p| p.slot == u.slot) {
            layout.push(RootParam {
                root,
                table_offset_dwords: 0,
                sgprs,
                base_register: u.lower_bound,
                user_data: u.user_data,
                slot: u.slot,
            });
        }
        used.push(u);
    }
    Ok(NativeProgram {
        stage: HwStage::Cs,
        wave64,
        vgprs,
        layout,
        used,
        io: StageIo::Cs(ComputeIo {
            rsrc2: u32_at(packet, PACKET_RSRC2)?,
            threads: [threads(0)?, threads(1)?, threads(2)?],
        }),
        code: code.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `FSPX` blob of the given entries (kind, data), laid out as the
    /// upscaler's are: the entry table, then each entry's data.
    fn blob(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut at = HEADER + ENTRY * entries.len();
        for (kind, data) in entries {
            for w in [*kind, at as u32, data.len() as u32, 0] {
                out.extend_from_slice(&w.to_le_bytes());
            }
            at += data.len();
        }
        for (_, data) in entries {
            out.extend_from_slice(data);
        }
        let size = out.len() as u32;
        out[12..16].copy_from_slice(&size.to_le_bytes());
        out
    }

    fn packet(threads: [u16; 3], wave32: bool, rsrc1: u32, rsrc2: u32) -> Vec<u8> {
        let mut p = vec![0; 0x1c];
        for (k, t) in threads.iter().enumerate() {
            p[PACKET_THREADS + 2 * k..][..2].copy_from_slice(&t.to_le_bytes());
        }
        let flags = if wave32 { PACKET_WAVE32 } else { 0 };
        p[PACKET_FLAGS..][..2].copy_from_slice(&flags.to_le_bytes());
        p[PACKET_RSRC1..][..4].copy_from_slice(&rsrc1.to_le_bytes());
        p[PACKET_RSRC2..][..4].copy_from_slice(&rsrc2.to_le_bytes());
        p
    }

    /// A resource record: type, root type, table offset, register,
    /// descriptor dwords, user-data dword, root slot.
    fn resource(
        kind: u8,
        root: u8,
        offset: u32,
        register: u32,
        dwords: u8,
        ud: u8,
        slot: u8,
    ) -> [u8; RECORD] {
        let mut r = [0; RECORD];
        r[0] = TAG_RESOURCE;
        r[4] = kind;
        r[5] = root;
        r[8..12].copy_from_slice(&offset.to_le_bytes());
        r[12..16].copy_from_slice(&register.to_le_bytes());
        r[20] = dwords;
        r[21] = ud;
        r[22] = slot;
        r[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        r
    }

    fn metadata(code_dwords: u32, records: &[[u8; RECORD]]) -> Vec<u8> {
        let mut m = vec![0; RECORDS];
        m[0..4].copy_from_slice(&0x403u32.to_le_bytes());
        m[META_CODE_DWORDS..][..4].copy_from_slice(&code_dwords.to_le_bytes());
        for r in records {
            m.extend_from_slice(r);
        }
        let mut hash = [0; RECORD];
        hash[0] = TAG_HASH;
        m.extend_from_slice(&hash);
        m
    }

    const CODE: [u8; 8] = [0x00, 0x00, 0x81, 0xBF, 0x00, 0x00, 0x81, 0xBF];

    #[test]
    fn a_compute_program_and_its_root_parameters_are_read() {
        let mut view = [0; RECORD];
        view[0] = TAG_VIEW;
        let records = [
            resource(2, 1, 0, 0, 4, 0, 0), // b0, a root view
            resource(0, 3, 0, 0, 8, 2, 1), // t0, a table
            view,
            resource(3, 3, 8, 0, 8, 2, 1),    // u0, the same table
            resource(1, 4, 0, 0, 4, 0xFF, 2), // s0, an immediate sampler
        ];
        let part = blob(&[
            (KIND_PACKET, packet([8, 8, 1], true, 0x0B, 0x1234)),
            (1, vec![0; 16]),
            (KIND_CODE, CODE.to_vec()),
            (KIND_METADATA, metadata(2, &records)),
        ]);
        let programs = crate::native_programs(&part).unwrap();
        let [p] = &programs[..] else {
            panic!("one program: {programs:?}")
        };
        assert_eq!(p.stage, HwStage::Cs);
        assert!(!p.wave64);
        assert_eq!(p.vgprs, 12 * 8);
        assert_eq!(p.code, CODE);
        assert_eq!(
            p.io,
            StageIo::Cs(ComputeIo {
                rsrc2: 0x1234,
                threads: [8, 8, 1]
            })
        );
        let roots: Vec<_> = p
            .layout
            .iter()
            .map(|r| (r.root, r.sgprs, r.user_data, r.slot))
            .collect();
        assert_eq!(
            roots,
            [(RootType::Descriptor, 2, 0, 0), (RootType::Table, 1, 2, 1)]
        );
        let kinds: Vec<_> = p
            .used
            .iter()
            .map(|u| (u.kind, u.table_offset_dwords, u.dwords))
            .collect();
        assert_eq!(
            kinds,
            [
                (ResourceType::Cbv, 0, 4),
                (ResourceType::Srv, 0, 8),
                (ResourceType::Uav, 8, 8),
                (ResourceType::Sampler, 0, 4)
            ]
        );
    }

    #[test]
    fn wave64_programs_count_vgprs_in_fours() {
        let part = blob(&[
            (KIND_PACKET, packet([64, 1, 1], false, 0x03, 0)),
            (KIND_CODE, CODE.to_vec()),
            (KIND_METADATA, metadata(2, &[])),
        ]);
        let p = program(&part).unwrap();
        assert!(p.wave64);
        assert_eq!(p.vgprs, 16);
    }

    #[test]
    fn malformed_blobs_are_refused() {
        let good = |code_dwords| {
            blob(&[
                (KIND_PACKET, packet([64, 1, 1], false, 0, 0)),
                (KIND_CODE, CODE.to_vec()),
                (KIND_METADATA, metadata(code_dwords, &[])),
            ])
        };
        assert!(program(&good(2)).is_ok());
        assert_eq!(
            program(&good(3)),
            Err(Malformed("FSPX code size disagrees"))
        );
        let mut version = good(2);
        version[4] = 2;
        assert_eq!(program(&version), Err(Malformed("unknown FSPX version")));
        let no_code = blob(&[
            (KIND_PACKET, packet([64, 1, 1], false, 0, 0)),
            (KIND_METADATA, metadata(2, &[])),
        ]);
        assert_eq!(program(&no_code), Err(Malformed("FSPX entry missing")));
        let mut far = good(2);
        far[HEADER + ENTRY + 4] = 0xF0; // the code entry's offset
        assert_eq!(program(&far), Err(Malformed("FSPX entry out of bounds")));
        let mut odd = [0; RECORD];
        odd[0] = 0x44;
        let unknown = blob(&[
            (KIND_PACKET, packet([64, 1, 1], false, 0, 0)),
            (KIND_CODE, CODE.to_vec()),
            (KIND_METADATA, metadata(2, &[odd])),
        ]);
        assert_eq!(
            program(&unknown),
            Err(Malformed("unknown FSPX metadata record"))
        );
    }
}
