//! End-to-end tests of a compute program's implicit-level samples (`lower::quad`): the level
//! comes from the quad of lanes, as on the console. Synthetic GFX10 compute programs
//! translated ([`super::emit`]), validated by `dxil.dll` when present, and run on a device whose
//! waves can be 32 lanes (the programs read the console's lanes, so they declare that size):
//! the hardware GPU; not WARP, which runs waves of another size (as for the other programs
//! with console lanes, `valu_tests`' permlane test among them).

use super::host::{Dispatch, Host, Root, View};
use super::{HostShader, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{Container, dxbc, io, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;

/// The lanes of the group (one wave of 32).
const LANES: u32 = 32;

/// A wave32 compute program of 32 threads (the thread's x in v0): 4 root constants in s0..s3
/// and, in s4, a table with a texture's descriptor at dword 0 and a raw UAV's at dword 8,
/// loaded into s[8:15] and s[16:19]; an immediate sampler in s[20:23] that filters points and
/// picks the nearest level; then `body`.
fn native(body: &[u32]) -> NativeProgram {
    let mut words = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0xF40C_0202,
        0xFA00_0000, // s_load_dwordx8 s[8:15], s[4:5], null
        0xF408_0402,
        0xFA00_0020, // s_load_dwordx4 s[16:19], s[4:5], 0x20
        0xBE94_0380, // s_mov_b32 s20, 0 (wrap)
        0xBE95_03FF,
        0x00C0_0000, // s_mov_b32 s21, 0x00c00000 (levels up to 12)
        0xBE96_03FF,
        0x0400_0000, // s_mov_b32 s22, 0x04000000 (points, the nearest level)
        0xBE97_0380, // s_mov_b32 s23, 0
    ];
    words.extend_from_slice(body);
    words.push(S_ENDPGM);
    let range = |kind, table_offset_dwords, dwords| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords,
        lower_bound: 0,
        stride: 0,
        dwords,
        user_data: 4,
        slot: 1,
    };
    NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
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
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used: vec![
            range(ResourceType::Srv, 0, 8),
            range(ResourceType::Uav, 8, 4),
        ],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1, // 5 user SGPRs: s0..s4
            threads: [LANES, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// `native` translated as the title's programs are, and validated.
fn translate(native: &NativeProgram) -> HostShader {
    let container = dxbc::build(&[]);
    let c = Container::parse(&container).expect("container");
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, native).expect("io::raise");
    let shader = super::emit(&c, native, &p, &st, &io, &res).expect("emit");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    } else {
        eprintln!("no dxil.dll; validation skipped");
    }
    shader
}

/// Runs `shader` over an 8x8 texture of four levels whose red is the level's number and a UAV
/// of a dword per lane at heap indices 0 and 1, on the hardware GPU and on WARP (each skipped
/// without a device or without waves of 32 lanes), and returns per one that ran the UAV's
/// dwords.
fn run(shader: &HostShader) -> Vec<(&'static str, Vec<u32>)> {
    let texels: Vec<Vec<[f32; 4]>> = (0..4)
        .map(|k| vec![[k as f32, 0.0, 0.0, 1.0]; (64 >> (2 * k)).max(1)])
        .collect();
    let levels: Vec<&[[f32; 4]]> = texels.iter().map(Vec::as_slice).collect();
    let views = [View::Mips(8, 8, &levels), View::RawUav(LANES)];
    let mut out = Vec::new();
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        // (the program reads the console's lanes: it declares waves of 32)
        if let Some((min, max)) = host.wave_lanes()
            && !(min..=max).contains(&32)
        {
            eprintln!("waves of {min} to {max} lanes on the {label} device: not run there");
            continue;
        }
        let (root, pso) = host
            .compute_pipeline(shader)
            .unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        let words = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &views,
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        out.push((label, words[0].clone()));
    }
    out
}

#[test]
fn a_compute_sample_takes_its_level_from_the_quad_of_lanes() {
    // u = lane / 8 across an 8-texel texture: the quad's lane 1 is a texel on from lane 0,
    // its lane 2 two (the console's derivatives: lane 1 minus lane 0 across, lane 2 minus
    // lane 0 down), so the level is log2(2) = 1. (Level 0 before.)
    let shader = translate(&native(&[
        0x7E02_0D00, // v_cvt_f32_u32 v1, v0
        0x1002_02FF,
        0x3E00_0000, // v_mul_f32 v1, 0.125, v1
        0x7E04_02F0, // v_mov_b32 v2, 0.5
        0xF080_0F08,
        0x00A2_0301, // image_sample v[3:6], v[1:2], s[8:15], s[20:23] dmask:0xf 2D
        0x340E_0082, // v_lshlrev_b32 v7, 2, v0
        0xE070_1000,
        0x8004_0307, // buffer_store_dword v3, v7, s[16:19], 0 offen (red: the level)
    ]));
    for (label, words) in run(&shader) {
        assert_eq!(words, vec![1.0f32.to_bits(); LANES as usize], "{label}");
    }
}

#[test]
fn a_compute_sample_with_a_bias_scales_its_quads_gradients() {
    // As above with a bias of 1: level 2 (the quad's gradients times 2).
    let shader = translate(&native(&[
        0x7E02_02F2, // v_mov_b32 v1, 1.0 (the bias)
        0x7E04_0D00, // v_cvt_f32_u32 v2, v0
        0x1004_04FF,
        0x3E00_0000, // v_mul_f32 v2, 0.125, v2
        0x7E06_02F0, // v_mov_b32 v3, 0.5
        0xF094_0F08,
        0x00A2_0401, // image_sample_b v[4:7], v[1:3], s[8:15], s[20:23] dmask:0xf 2D
        0x340E_0082, // v_lshlrev_b32 v7, 2, v0
        0xE070_1000,
        0x8004_0407, // buffer_store_dword v4, v7, s[16:19], 0 offen (red: the level)
    ]));
    for (label, words) in run(&shader) {
        assert_eq!(words, vec![2.0f32.to_bits(); LANES as usize], "{label}");
    }
}
