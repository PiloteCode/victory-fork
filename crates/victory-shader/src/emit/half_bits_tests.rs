//! 16-bit typed buffer accesses through unsigned views (`lower`'s `half_bits`; fxceless #927,
//! tippythetaidum): synthetic programs, lowered, validated by `dxil.dll` when present, and run
//! on the hardware adapter and on WARP (with `VICTORY_WARP=1`, on WARP alone).
//!
//! On the card, where the old form read 0 (the title's face network computed zeros):
//! `cargo test -p victory-shader half_bits_tests -- --test-threads=1 --nocapture`.

use super::host::{Dispatch, Host, Root, Stride, View};
use super::lower::with_half_bits_off;
use super::valu_tests::ONE_AT_A_TIME;
use super::{HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16_UINT, DXGI_FORMAT_R16G16B16A16_UINT};

const S_ENDPGM: u32 = 0xBF81_0000;
const LOAD_FORMAT_X: u32 = 0x00;
const LOAD_FORMAT_D16_X: u32 = 0x80;
const LOAD_FORMAT_D16_XYZW: u32 = 0x83;
const STORE_FORMAT_D16_XYZW: u32 = 0x87;
const V_CVT_F32_F16: u16 = 0x0B;
const V_ADD_F32: u8 = 0x03;

/// SOP1 `s_mov_b32 sdst, src0`.
const fn s_mov_b32(sdst: u8, src0: u8) -> u32 {
    0xBE80_0000 | (sdst as u32) << 16 | 0x03 << 8 | src0 as u32
}

/// SMEM `s_load_dwordx4 s[dst:dst+3], s[4:5], offset` (soffset null): the table's descriptor at
/// byte `offset`.
const fn s_load_dwordx4(dst: u8, offset: u32) -> [u32; 2] {
    [
        0x3D << 26 | 0x02 << 18 | (dst as u32) << 6 | (4 >> 1),
        125 << 25 | offset,
    ]
}

/// SMEM `s_load_dwordx4 s[dst:dst+3], s[4:5], soffset`: the offset in an SGPR.
const fn s_load_dwordx4_at(dst: u8, soffset: u8) -> [u32; 2] {
    [
        0x3D << 26 | 0x02 << 18 | (dst as u32) << 6 | (4 >> 1),
        (soffset as u32) << 25,
    ]
}

/// VOP1 `op vdst, src0` (`src0`: 0..105 = SGPR, 128 = 0, 256..511 = VGPR).
const fn vop1(op: u16, vdst: u8, src0: u16) -> u32 {
    0x7E00_0000 | (vdst as u32) << 17 | (op as u32) << 9 | (src0 as u32 & 0x1FF)
}

/// VOP2 `op vdst, src0, vsrc1`.
const fn vop2(op: u8, vdst: u8, src0: u16, vsrc1: u8) -> u32 {
    (op as u32 & 0x3F) << 25 | (vdst as u32) << 17 | (vsrc1 as u32) << 9 | (src0 as u32 & 0x1FF)
}

const fn vgpr(n: u8) -> u16 {
    256 + n as u16
}

/// MUBUF `op vdata, vaddr, s[srsrc:srsrc+3], 0 idxen`.
const fn mubuf_idxen(op: u32, vdata: u8, vaddr: u8, srsrc: u8) -> [u32; 2] {
    [
        0x38 << 26 | op << 18 | 1 << 13,
        0x80 << 24 | ((srsrc as u32 >> 2) & 0x1F) << 16 | (vdata as u32) << 8 | vaddr as u32,
    ]
}

/// MUBUF `buffer_store_dword vdata, off, s[srsrc:srsrc+3], 0`.
const fn buffer_store_dword(vdata: u8, srsrc: u8) -> [u32; 2] {
    [
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | ((srsrc as u32 >> 2) & 0x1F) << 16 | (vdata as u32) << 8,
    ]
}

/// A used-resource entry of the table in root parameter 1.
fn entry(kind: ResourceType, table_offset_dwords: u32, stride: u32) -> UsedResource {
    UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords,
        lower_bound: table_offset_dwords / 8,
        stride,
        dwords: 4,
        user_data: 4,
        slot: 1,
    }
}

/// `code` lowered as a compute program of `threads` threads (the thread's index in v0) whose
/// root parameter 1 is a table (its address in s4, user data 4) with the entries `used`;
/// validated when `dxil.dll` is there.
fn lowered(code: &[u32], threads: u32, used: Vec<UsedResource>) -> HostShader {
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 8,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used,
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1,
            threads: [threads, 1, 1],
        }),
        code: code.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res =
        resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resource::track");
    let io = io::raise(&p, &native).expect("io::raise");
    let l = lower::lower(&lower::Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    })
    .expect("lower");
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: l.dxil,
        user_data: l.user_data,
        samplers: l.samplers,
        static_samplers: Vec::new(),
        root_cbvs: l.root_cbvs,
        root_srvs: l.root_srvs,
        root_uavs: l.root_uavs,
        root_indexed_views: res.root_indexed_views,
        half_views: l.half_views,
        atomic_views: l.atomic_views,
        written_views: l.written_views,
        picked_views: l.picked_views,
    };
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    }
    shader
}

/// The devices to run on: the hardware adapter and WARP, or WARP alone with `VICTORY_WARP=1`
/// (which makes the default adapter WARP too); those that are there.
fn devices() -> Vec<(&'static str, Host)> {
    let warp_only = std::env::var("VICTORY_WARP").is_ok_and(|v| v == "1");
    let mut out = Vec::new();
    for (label, warp) in [("hardware", false), ("warp", true)] {
        if warp_only && !warp {
            continue;
        }
        match Host::new(warp) {
            Ok(host) => out.push((label, host)),
            Err(e) => eprintln!("no {label} device ({e}); skipped"),
        }
    }
    assert!(!out.is_empty(), "no device to run on");
    out
}

/// `shader` run once on `host` over `views` (the table at heap index 0): each UAV's dwords.
fn run(label: &str, host: &Host, shader: &HostShader, views: &[View]) -> Vec<Vec<u32>> {
    let (root, pso) = host
        .compute_pipeline(shader)
        .unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
    let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    for u in &shader.user_data {
        ud[u.index as usize] = match (u.root, u.slot) {
            (RootType::Table, 1) => 0,
            other => panic!("unexpected user data {other:?}"),
        };
    }
    host.dispatch(
        &root,
        &pso,
        &Dispatch {
            roots: vec![(0, Root::Constants(ud))],
            views,
            samplers: &[],
            groups: [1, 1, 1],
        },
    )
    .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"))
}

/// `buffer_load_format_d16_xyzw` from the table's first view and `buffer_store_format_d16_xyzw`
/// to its second, through the title's kind of views (`R16G16B16A16_UINT`), move every 16-bit
/// pattern as it is, as the console does: half floats, the not-a-number ones among them, and
/// plain 16-bit integers past the largest half's pattern (the title keeps indices beside its
/// halves in such buffers). The program names both places for the host
/// (`HostShader::half_views`). With `VICTORY_HALF_BITS=0` it names none and moves the channels
/// by way of floats, as before: what that gives through these views is outside what D3D12
/// defines, and is printed, not checked (#927: every pattern read back as 0 on NVIDIA's cards).
#[test]
fn d16_typed_accesses_through_unsigned_views_keep_every_pattern() {
    const THREADS: u32 = 1024;
    let mut code = vec![s_mov_b32(5, 0x80)];
    code.extend_from_slice(&s_load_dwordx4(8, 0));
    code.extend_from_slice(&s_load_dwordx4(12, 0x20));
    // v[1:2] = first[thread] (four halves), second[thread] = v[1:2].
    code.extend_from_slice(&mubuf_idxen(LOAD_FORMAT_D16_XYZW, 1, 0, 8));
    code.extend_from_slice(&mubuf_idxen(STORE_FORMAT_D16_XYZW, 1, 0, 12));
    code.push(S_ENDPGM);
    let used = || {
        vec![
            entry(ResourceType::Srv, 0, 4),
            entry(ResourceType::Uav, 8, 4),
        ]
    };
    let new = lowered(&code, THREADS, used());
    assert_eq!(new.half_views, [(1, 0), (1, 8)]);
    let old = with_half_bits_off(|| lowered(&code, THREADS, used()));
    assert!(old.half_views.is_empty(), "the switch off names nothing");
    assert_ne!(new.dxil, old.dxil);
    // Every 16-bit pattern, 4,096 a dispatch (four a thread).
    let data: Vec<u32> = (0..=u16::MAX)
        .step_by(2)
        .map(|p| u32::from(p) | u32::from(p + 1) << 16)
        .collect();
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for (label, host) in devices() {
        let mut came_back = [0usize; 2];
        let mut zeros = 0usize;
        for chunk in data.chunks(2 * THREADS as usize) {
            for (form, shader) in [&new, &old].into_iter().enumerate() {
                let out = run(
                    label,
                    &host,
                    shader,
                    &[
                        View::Buffer {
                            data: chunk,
                            stride: Stride::Format(DXGI_FORMAT_R16G16B16A16_UINT, 8),
                        },
                        View::HalfUintUav(THREADS),
                    ],
                );
                let halves = |words: &[u32]| -> Vec<u16> {
                    words
                        .iter()
                        .flat_map(|w| [*w as u16, (w >> 16) as u16])
                        .collect()
                };
                let (want, got) = (halves(chunk), halves(&out[0]));
                came_back[form] += want.iter().zip(&got).filter(|(a, b)| a == b).count();
                if form == 1 {
                    zeros += got.iter().filter(|&&h| h == 0).count();
                }
                if form == 0 && got != want {
                    let (k, (a, b)) = want
                        .iter()
                        .zip(&got)
                        .enumerate()
                        .find(|(_, (a, b))| a != b)
                        .expect("a difference");
                    panic!("{label}: half {k} of a dispatch came back as {b:#06x}, not {a:#06x}");
                }
            }
        }
        eprintln!(
            "{label}: through R16G16B16A16_UINT views, {} of 65536 16-bit patterns came back unchanged moved as integers; {} by way of floats (VICTORY_HALF_BITS=0, the old form; {zeros} read back as 0)",
            came_back[0], came_back[1]
        );
        assert_eq!(came_back[0], 65_536, "{label}: patterns unchanged");
    }
}

/// The places a program that moves halves names are the descriptors it loads, not the start of
/// the range their used-resource entry describes: with one entry for a range from dword 0, a
/// load at byte 0x20 names dword 8 (a host view there in a 16-bit float format is the one shown
/// as its unsigned twin). A descriptor picked at run time (the load's offset in an SGPR) names
/// nothing and keeps the float path: which slot it reads, and in which format, is known only
/// then. (The case avt0m4t48 found in review on #927.)
#[test]
fn half_places_are_the_descriptors_the_program_loads() {
    let used = || {
        vec![
            entry(ResourceType::Srv, 0, 4),
            entry(ResourceType::Uav, 16, 4),
        ]
    };
    let mut fixed = vec![s_mov_b32(5, 0x80)];
    fixed.extend_from_slice(&s_load_dwordx4(8, 0x20));
    fixed.extend_from_slice(&s_load_dwordx4(12, 0x40));
    fixed.extend_from_slice(&mubuf_idxen(LOAD_FORMAT_D16_XYZW, 1, 0, 8));
    fixed.extend_from_slice(&mubuf_idxen(STORE_FORMAT_D16_XYZW, 1, 0, 12));
    fixed.push(S_ENDPGM);
    assert_eq!(lowered(&fixed, 32, used()).half_views, [(1, 8), (1, 16)]);
    // The load's offset in s20, from the first lane's thread index (`v_readfirstlane_b32 s20,
    // v0`): picked at run time. The pick may land on the store's place too, so no place of that
    // table is named: the d16 accesses there take the float path, as before.
    let mut runtime = vec![s_mov_b32(5, 0x80), vop1(0x02, 20, vgpr(0))];
    runtime.extend_from_slice(&s_load_dwordx4_at(8, 20));
    runtime.extend_from_slice(&s_load_dwordx4(12, 0x40));
    runtime.extend_from_slice(&mubuf_idxen(LOAD_FORMAT_D16_XYZW, 1, 0, 8));
    runtime.extend_from_slice(&mubuf_idxen(STORE_FORMAT_D16_XYZW, 1, 0, 12));
    runtime.push(S_ENDPGM);
    assert!(lowered(&runtime, 32, used()).half_views.is_empty());
}

/// A format load of whole dwords (`buffer_load_format_x`, not d16) whose result the program
/// only reads one half of as a half float (`v_cvt_f32_f16`) is moving 16-bit integers: the
/// head program reads the face network's output so, from an `R16_UINT` view. It names its view
/// for the host and loads the integer: 0x3c00 comes through as 1.0, straight, and behind a
/// branch with a later masked rewrite of the register. Used as a whole float instead
/// (`v_add_f32`), the load stays a float load and names nothing; so does a load of halves
/// beside a load of the same view read as a whole float (a view of numbers the program also
/// picks one half float out of, as hundreds of the title's pixel programs do), and a d16 load
/// there. A d16 load alone names its place; through the `R16_UINT` view it reads 1.0 too
/// (printed for the old form, by way of floats).
#[test]
fn a_format_load_read_as_halves_loads_the_integer() {
    let program = |body: &[u32]| {
        let mut code = vec![s_mov_b32(5, 0x80)];
        code.extend_from_slice(&s_load_dwordx4(8, 0));
        code.extend_from_slice(&s_load_dwordx4(12, 0x20));
        // `body` leaves v2, stored raw.
        code.extend_from_slice(body);
        code.extend_from_slice(&buffer_store_dword(2, 12));
        code.push(S_ENDPGM);
        lowered(
            &code,
            1,
            vec![
                entry(ResourceType::Srv, 0, 4),
                entry(ResourceType::Uav, 8, 0),
            ],
        )
    };
    // v1 = first[v0] (one channel).
    let load = mubuf_idxen(LOAD_FORMAT_X, 1, 0, 8);
    let half = vop1(V_CVT_F32_F16, 2, vgpr(1));
    let as_halves = program(&[&load[..], &[half]].concat());
    assert_eq!(as_halves.half_views, [(1, 0)]);
    let as_float = program(&[&load[..], &[vop2(V_ADD_F32, 2, vgpr(1), 1)]].concat());
    assert!(as_float.half_views.is_empty());
    // v3 = first[v0] too, read as a whole float: v2 = half(v1) + v3.
    let mut beside = load.to_vec();
    beside.extend_from_slice(&mubuf_idxen(LOAD_FORMAT_X, 3, 0, 8));
    beside.push(half);
    beside.push(vop2(V_ADD_F32, 2, vgpr(3), 2));
    assert!(program(&beside).half_views.is_empty());
    // Behind a branch, as the head program has it: where the lanes meet again the loaded value
    // goes on through a phi, and a later write of v1 under EXEC reads it only to keep it in the
    // lanes it leaves alone.
    const SAVE_EXEC: u32 = 0xBE90_037E; // s_mov_b32 s16, exec_lo
    const ONLY_LANE_0: u32 = 0x7DA4_0080; // v_cmpx_eq_u32 exec_lo, 0, v0
    const SKIP_3: u32 = 0xBF88_0003; // s_cbranch_execz over the next 3 words
    const RESTORE_EXEC: u32 = 0xBEFE_0310; // s_mov_b32 exec_lo, s16
    let zero = vop1(0x01, 2, 128); // v_mov_b32 v2, 0
    let rewrite = vop1(0x01, 1, 128); // v_mov_b32 v1, 0
    let branched = program(
        &[
            &[zero, SAVE_EXEC, ONLY_LANE_0, SKIP_3][..],
            &load,
            &[
                half,
                RESTORE_EXEC,
                SAVE_EXEC,
                ONLY_LANE_0,
                rewrite,
                RESTORE_EXEC,
            ],
        ]
        .concat(),
    );
    assert_eq!(branched.half_views, [(1, 0)]);
    // A d16 load names its place where it is the only access there...
    let d16 = mubuf_idxen(LOAD_FORMAT_D16_X, 1, 0, 8);
    let d16_body = [&d16[..], &[half]].concat();
    let d16_alone = program(&d16_body);
    assert_eq!(d16_alone.half_views, [(1, 0)]);
    // ...and not beside a whole float read through the same place, which would see the
    // unsigned twin: v2 = half(d16 v1) + v3.
    let mut d16_beside = d16.to_vec();
    d16_beside.extend_from_slice(&mubuf_idxen(LOAD_FORMAT_X, 3, 0, 8));
    d16_beside.push(half);
    d16_beside.push(vop2(V_ADD_F32, 2, vgpr(3), 2));
    assert!(program(&d16_beside).half_views.is_empty());
    // The switch off: nothing named.
    let d16_old = with_half_bits_off(|| program(&d16_body));
    assert!(d16_old.half_views.is_empty());
    let as_halves_old = with_half_bits_off(|| program(&[&load[..], &[half]].concat()));
    assert!(as_halves_old.half_views.is_empty());
    // Element 0 of an R16_UINT buffer holds the half 1.0 (0x3c00).
    let data = [0x0000_3c00u32, 0];
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for (label, host) in devices() {
        let read = |shader: &HostShader| {
            run(
                label,
                &host,
                shader,
                &[
                    View::Buffer {
                        data: &data,
                        stride: Stride::Format(DXGI_FORMAT_R16_UINT, 2),
                    },
                    View::RawUav(1),
                ],
            )[0][0]
        };
        for (name, shader) in [
            ("straight", &as_halves),
            ("branched", &branched),
            ("d16", &d16_alone),
        ] {
            assert_eq!(
                read(shader),
                1.0f32.to_bits(),
                "{label}, {name}: the half read as an integer's bits"
            );
        }
        eprintln!(
            "{label}: the old form (VICTORY_HALF_BITS=0) read 0x3c00 through an R16_UINT view as {} with buffer_load_format_d16_x, as {} with buffer_load_format_x (1.0 is right)",
            f32::from_bits(read(&d16_old)),
            f32::from_bits(read(&as_halves_old)),
        );
    }
}

/// The places a program uses buffers with atomics through and writes (`HostShader::atomic_views`
/// and `written_views`) are the descriptors it loads from fixed places of a table, as for
/// halves: a `buffer_atomic_add` through the descriptor at byte 0x20 and a `buffer_store_dword`
/// through the one at 0x40 name dwords 8 (atomic, and written) and 16 (written); a load through
/// a third names nothing. The atomic's descriptor loaded at an offset the program computes
/// names its table only (`picked_views`, both bits). The lists change nothing written: the
/// program's DXIL is the same with any of them.
#[test]
fn atomic_and_written_places_are_the_descriptors_the_program_loads() {
    const ATOMIC_ADD: u32 = 0x32;
    let used = || {
        vec![
            entry(ResourceType::Uav, 0, 4),
            entry(ResourceType::Uav, 16, 4),
            entry(ResourceType::Srv, 24, 4),
        ]
    };
    let mut fixed = vec![s_mov_b32(5, 0x80)];
    fixed.extend_from_slice(&s_load_dwordx4(8, 0x20));
    fixed.extend_from_slice(&s_load_dwordx4(12, 0x40));
    fixed.extend_from_slice(&s_load_dwordx4(16, 0x60));
    fixed.extend_from_slice(&mubuf_idxen(ATOMIC_ADD, 1, 0, 8));
    fixed.extend_from_slice(&buffer_store_dword(1, 12));
    fixed.extend_from_slice(&mubuf_idxen(LOAD_FORMAT_X, 2, 0, 16));
    fixed.push(S_ENDPGM);
    let s = lowered(&fixed, 32, used());
    assert_eq!(s.atomic_views, [(1, 8)]);
    assert_eq!(s.written_views, [(1, 8), (1, 16)]);
    assert!(s.picked_views.is_empty());
    let census = super::take_view_census();
    assert_eq!((census[0], census[3], census[6], census[7]), (1, 1, 0, 0), "{census:?}");
    let mut runtime = vec![s_mov_b32(5, 0x80), vop1(0x02, 20, vgpr(0))];
    runtime.extend_from_slice(&s_load_dwordx4_at(8, 20));
    runtime.extend_from_slice(&s_load_dwordx4(12, 0x40));
    runtime.extend_from_slice(&mubuf_idxen(ATOMIC_ADD, 1, 0, 8));
    runtime.extend_from_slice(&buffer_store_dword(1, 12));
    runtime.push(S_ENDPGM);
    let s = lowered(&runtime, 32, used());
    assert!(s.atomic_views.is_empty());
    assert_eq!(s.written_views, [(1, 16)]);
    assert_eq!(s.picked_views, [(1, super::PICKED_ATOMIC | super::PICKED_WRITTEN)]);
    let census = super::take_view_census();
    assert_eq!((census[1], census[3]), (1, 1), "{census:?}");
}
