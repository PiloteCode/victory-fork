//! The forms of the FidelityFX upscaler's programs (`lower::upscaler`, `native::fspx`,
//! `resource::upscaler`): hand-written programs in an `ACCC` container and as native programs,
//! translated for a host of 32-lane waves, validated by `dxil.dll` when present, and run on
//! the test device when it takes them (`VICTORY_WARP=1`: WARP, whose waves are 4 lanes, runs
//! the programs that ask for no wave size).

use super::host::{Dispatch, Host, Root, View};
use super::wave::{Op, Shape, WAVE_OR_V7_TO_S7, sections};
use super::{HostShader, lower, validator};
use crate::dxbc::FourCC;
use crate::ir::Reg;
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{ShaderError, io, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;
const LIT: u32 = 255;
const SRC_SCC: u32 = 253;

const fn v(n: u32) -> u32 {
    256 + n
}

const fn k(n: u32) -> u32 {
    128 + n
}

const fn vop1(op: u32, vdst: u32, src0: u32) -> u32 {
    0x7E00_0000 | vdst << 17 | op << 9 | src0
}

const fn vop2(op: u32, vdst: u32, src0: u32, vsrc1: u32) -> u32 {
    op << 25 | vdst << 17 | vsrc1 << 9 | src0
}

const fn vop3(op: u32, dst: u32, src0: u32, src1: u32) -> [u32; 2] {
    [0xD400_0000 | op << 16 | dst, src0 | src1 << 9]
}

/// `buffer_store_dword v<data>, v<addr>, s[8:11], 0 offen offset:<offset>`
const fn store(data: u32, addr: u32, offset: u32) -> [u32; 2] {
    [0xE070_1000 | offset, 0x8002_0000 | data << 8 | addr]
}

fn bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// An `FSPX` blob as the upscaler's compiler lays one out: the pipeline registers (`threads`
/// in a group, waves of 32, `rsrc2`), the code, and the program metadata with one record per
/// resource: (resource type, root type, table offset in dwords, descriptor dwords, user-data
/// dword, root slot).
fn fspx(
    threads: [u16; 3],
    rsrc2: u32,
    code: &[u8],
    resources: &[(u8, u8, u32, u8, u8, u8)],
) -> Vec<u8> {
    let mut packet = vec![0u8; 0x1c];
    for (n, t) in threads.iter().enumerate() {
        packet[0x0c + 2 * n..][..2].copy_from_slice(&t.to_le_bytes());
    }
    packet[0x12..0x14].copy_from_slice(&(1u16 << 4).to_le_bytes());
    packet[0x18..0x1c].copy_from_slice(&rsrc2.to_le_bytes());
    let mut meta = vec![0u8; 0x18];
    meta[0..4].copy_from_slice(&0x403u32.to_le_bytes());
    meta[8..12].copy_from_slice(&(code.len() as u32 / 4).to_le_bytes());
    for &(kind, root, offset, dwords, ud, slot) in resources {
        let mut r = [0u8; 32];
        r[0] = 0x41;
        r[4] = kind;
        r[5] = root;
        r[8..12].copy_from_slice(&offset.to_le_bytes());
        r[20] = dwords;
        r[21] = ud;
        r[22] = slot;
        meta.extend_from_slice(&r);
    }
    let mut hash = [0u8; 32];
    hash[0] = 0x43;
    meta.extend_from_slice(&hash);
    let entries = [(0u32, packet), (6, code.to_vec()), (7, meta)];
    // "FSPX", version 1, the entry count, the size (set last)
    let mut out = b"FSPX".to_vec();
    for w in [1, entries.len() as u32, 0] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    let mut at = 0x10 + 16 * entries.len();
    for (kind, data) in &entries {
        for w in [*kind, at as u32, data.len() as u32, 0] {
            out.extend_from_slice(&w.to_le_bytes());
        }
        at += data.len();
    }
    for (_, data) in &entries {
        out.extend_from_slice(data);
    }
    let size = out.len() as u32;
    out[12..16].copy_from_slice(&size.to_le_bytes());
    out
}

/// `container` translated for a host of 32-lane waves, validated when `dxil.dll` is there.
fn translated(container: &[u8]) -> Result<HostShader, ShaderError> {
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let shader = super::translate(container);
    lower::TEST_HOST_WAVE.set(was);
    let shader = shader?;
    if let Ok(mut val) = validator::Validator::load(validator::Validator::default_path().as_deref())
    {
        let mut copy = shader.dxil.clone();
        val.validate(&mut copy).expect("DXIL validation failed");
    }
    Ok(shader)
}

/// `shader` run once on the test device, one group, its tables at 0 and its root constants
/// `constants`, with `views`: each UAV's dwords, or `None` (and a line) where the device does
/// not take the program.
fn run(shader: &HostShader, constants: &[u32], views: &[View]) -> Option<Vec<Vec<u32>>> {
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 device; not run");
        return None;
    };
    let (root, pso) = match host.compute_pipeline(shader) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("the device does not take this program: {e}; not run");
            return None;
        }
    };
    let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    for u in &shader.user_data {
        if u.root == RootType::Constants {
            ud[u.index as usize] = constants.get(u.dword as usize).copied().unwrap_or(0);
        }
    }
    let out = host
        .dispatch(
            &root,
            &pso,
            &Dispatch {
                roots: vec![(0, Root::Constants(ud))],
                views,
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
    Some(out)
}

/// An `ACCC` container (no `DXIL` part) of a program of 32 threads in a wave of 32 with a
/// raw UAV in its table at user-data dword 4: the byte dot products (`v_dot4c_i32_i8`
/// accumulating into its destination, `v_dot4_i32_i8` adding its third source) and SCC as a
/// vector operand (`src_scc`), each stored for each thread.
#[test]
fn a_program_in_an_accc_part_translates_and_runs() {
    let mut w = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0xF408_0202,
        0xFA00_0000,               // s_load_dwordx4 s[8:11], s[4:5], 0
        0xBF8C_C07F,               // s_waitcnt lgkmcnt(0)
        vop2(0x1A, 1, k(2), v(0)), // v_lshlrev_b32 v1, 2, v0
        vop1(0x01, 2, LIT),        // v_mov_b32 v2, 0x80ff0201: the bytes 1, 2, -1, -128
        0x80FF_0201,
    ];
    w.extend(vop3(0x169, 6, v(0), LIT)); // v_mul_lo_u32 v6, v0, 0x01010101: each byte t
    w.extend([
        0x0101_0101,
        vop1(0x01, 3, k(5)),    // v_mov_b32 v3, 5
        vop2(0x0D, 3, v(2), 6), // v_dot4c_i32_i8 v3, v2, v6
        0xCC16_4004,
        0x1800_0000 | v(3) << 18 | v(6) << 9 | v(2), // v_dot4_i32_i8 v4, v2, v6, v3
        0xBF06_0505,                                 // s_cmp_eq_u32 s5, s5
        vop1(0x01, 5, SRC_SCC),                      // v_mov_b32 v5, src_scc
    ]);
    w.extend(store(3, 1, 0));
    w.extend(store(4, 1, 128));
    w.extend(store(5, 1, 256));
    w.push(S_ENDPGM);
    let blob = fspx([32, 1, 1], 5 << 1, &bytes(&w), &[(3, 3, 0, 4, 4, 0)]);
    let container = crate::dxbc::build(&[(FourCC::ACCC, &blob)]);
    let c = crate::Container::parse(&container).unwrap();
    assert!(c.part(FourCC::DXIL).is_none());
    let programs = crate::native_programs(c.native_part().unwrap().data).unwrap();
    assert_eq!(programs.len(), 1);
    assert_eq!(
        (programs[0].stage, programs[0].wave64),
        (HwStage::Cs, false)
    );
    let shader = translated(&container).expect("translates");
    let Some(out) = run(&shader, &[], &[View::RawUav(96)]) else {
        return;
    };
    let want: Vec<u32> = [|t: i32| 5 - 126 * t, |t| 5 - 252 * t, |_| 1]
        .iter()
        .flat_map(|f| (0..32).map(|t| f(t) as u32))
        .collect();
    assert_eq!(out[0], want);
    eprintln!("ran on the test device: byte dot products and src_scc as the console computes them");
}

/// The upscaler counts its groups in a texture with atomics and sets the counter back with a
/// store of one channel, 16 bits wide (after avatarmanz's translator, 389db9ef): one thread
/// adds 3, stores 9 (with and without d16), adds 1. A 16-bit store of a value that is not a
/// small constant is refused (the view's sign would extend it).
#[test]
fn a_counter_texture_is_set_by_a_store_of_one_channel() {
    let program = |store: [u32; 2], value: u32| {
        let w = [
            0xBE85_0380, // s_mov_b32 s5, 0
            0xF40C_0202,
            0xFA00_0000, // s_load_dwordx8 s[8:15], s[4:5], 0
            0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
            0x7E0A_0300, // v_mov_b32 v5, v0
            0x7E00_0280, // v_mov_b32 v0, 0
            0x7E02_0280, // v_mov_b32 v1, 0
            0x7E04_0283, // v_mov_b32 v2, 3
            0xF044_3108, // image_atomic_add v2, v[0:1], s[8:15] dmask:0x1 2D glc
            0x0002_0200,
            value, // v_mov_b32 v3, ..
            store[0],
            store[1],
            0x7E08_0281, // v_mov_b32 v4, 1
            0xF044_3108, // image_atomic_add v4, v[0:1], s[8:15] dmask:0x1 2D glc
            0x0002_0400,
            S_ENDPGM,
        ];
        let blob = fspx([1, 1, 1], 5 << 1, &bytes(&w), &[(3, 3, 0, 8, 4, 0)]);
        crate::dxbc::build(&[(FourCC::ACCC, &blob)])
    };
    // image_store v3, v[0:1], s[8:15] dmask:0x1 2D, without and with d16
    for store in [[0xF020_1108, 0x0002_0300], [0xF020_1108, 0x8002_0300]] {
        let shader = translated(&program(store, vop1(0x01, 3, k(9)))).expect("translates");
        let Some(out) = run(&shader, &[], &[View::UintTextureUav(1, 1)]) else {
            continue;
        };
        assert_eq!(out[0][0], 10, "the counter after 3, then 9, then 1");
    }
    let lane = translated(&program([0xF020_1108, 0x8002_0300], vop1(0x01, 3, v(5))));
    assert!(
        matches!(lane, Err(ShaderError::Unsupported { what, .. }) if what == "a 16-bit image store of one channel"),
        "{lane:?}"
    );
}

/// A wave64 compute program of 64 threads with 4 root constants in s0..s3 and a raw UAV's
/// descriptor in s[8:11], then `body`.
fn native(body: &[u32]) -> NativeProgram {
    let mut words = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0xF408_0202,
        0xFA00_0000, // s_load_dwordx4 s[8:11], s[4:5], 0
    ];
    words.extend_from_slice(body);
    words.push(S_ENDPGM);
    let table = |user_data, slot| RootParam {
        root: RootType::Table,
        table_offset_dwords: 0,
        sgprs: 1,
        base_register: 0,
        user_data,
        slot,
    };
    NativeProgram {
        stage: HwStage::Cs,
        wave64: true,
        vgprs: 16,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            table(4, 1),
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1,
            threads: [64, 1, 1],
        }),
        code: bytes(&words),
    }
}

/// "Some lane is left" of a mask operation as a number (after avatarmanz's translator,
/// 389db9ef): the upscaler's programs wait for other groups in a loop that ends when every
/// lane saw its flag, or after enough rounds: both tests become -1 or 0 and SCC of
/// `timeout | ~waiting` ends it. On a host of 32-lane waves the console wave is one host wave
/// (the paired form) or two that exchange.
#[test]
fn some_lane_of_a_mask_is_a_number_the_whole_wave_agrees_on() {
    let native = native(&[
        0xD765_0001,
        0x0001_00C1, // v_mbcnt_lo_u32_b32 v1, -1, 0
        0xD766_0001,
        0x0002_02C1, // v_mbcnt_hi_u32_b32 v1, -1, v1
        0x7D84_0200, // v_cmp_eq_u32 vcc, s0, v1
        0x8792_7E6A, // s_and_b64 s[18:19], vcc, exec
        0x8592_80C1, // s_cselect_b64 s[18:19], -1, 0
        0xBF09_8501, // s_cmp_ge_u32 s1, 5
        0x8594_80C1, // s_cselect_b64 s[20:21], -1, 0
        0x8B92_1214, // s_orn2_b64 s[18:19], s[20:21], s[18:19]
        0x8506_8081, // s_cselect_b32 s6, 1, 0
        0x7E04_0206, // v_mov_b32 v2, s6
        0xE070_0000,
        0x8002_0200, // buffer_store_dword v2, off, s[8:11], 0
    ]);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res =
        resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resource::track");
    let io = io::raise(&p, &native).expect("io::raise");
    let bytes = crate::dxbc::build(&[]);
    let c = super::Container::parse(&bytes).expect("container");
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let shader = super::emit(&c, &native, &p, &st, &io, &res);
    lower::TEST_HOST_WAVE.set(was);
    let shader = shader.expect("translates");
    let form = super::forms::form_of(&shader.dxil);
    assert!(!form.is_empty() && form != super::forms::WAVE64, "{form}");
    if let Ok(mut val) = validator::Validator::load(validator::Validator::default_path().as_deref())
    {
        let mut copy = shader.dxil.clone();
        val.validate(&mut copy).expect("DXIL validation failed");
    }
    // (lane s0 waits, if there is one; s1 rounds)
    for (wait, rounds, done) in [
        (0, 0, 0),
        (37, 4, 0),
        (63, 4, 0),
        (64, 0, 1),
        (37, 5, 1),
        (64, 9, 1),
    ] {
        let Some(out) = run(&shader, &[wait, rounds], &[View::RawUav(1)]) else {
            return;
        };
        assert_eq!(out[0][0], done, "lane {wait} after {rounds} rounds");
    }
}

/// A wave OR whose last step stays in the section's VGPR: the lower lanes add the first
/// half's to themselves, so only the upper ones hold the whole, and the last lane is read
/// after the section (the upscaler's programs; after avatarmanz's translator, e5841a05).
#[test]
fn one_lane_of_what_a_section_leaves_is_read_after_it() {
    let words = |lane: u32| {
        let mut w = WAVE_OR_V7_TO_S7[..14].to_vec(); // .. v_or_b32 v1, v2, v1
        w.extend([
            0xD760_0007,
            0x0001_0101, // v_readlane_b32 s7, v1, 0
            0x3802_0207, // v_or_b32 v1, s7, v1
            0xBEFE_0404, // s_mov_b64 exec, s[4:5]
            0xD760_0006,
            0x0001_0101 | lane << 9, // v_readlane_b32 s6, v1, lane
            S_ENDPGM,
        ]);
        w
    };
    let read = |lane: u32| {
        let p = ir::build(&bytes(&words(lane)), true, 256).unwrap();
        let insts = &p.blocks[0].insts;
        let saved = insts[0].write(Reg::S(4)).unwrap();
        let v7 = p
            .inputs
            .iter()
            .find_map(|&(r, v)| (r == Reg::V(7)).then_some(v))
            .unwrap();
        let got = insts[insts.len() - 2].write(Reg::S(6)).unwrap();
        let sec = &sections(&p, 0)[&0];
        (
            sec.reads.get(&got).copied(),
            Shape::Active(Op::Or, v7, saved, 0),
        )
    };
    let (found, whole) = read(63);
    assert_eq!(found, Some(whole));
    // Lane 0 has the first half's alone: still a lane read.
    assert_eq!(read(0).0, None);
}
