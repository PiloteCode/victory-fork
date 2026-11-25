//! Pixel programs the console runs after the depth test
//! (`DB_SHADER_CONTROL.DEPTH_BEFORE_SHADER`) declare it to the host
//! (`ForceEarlyDepthStencil`, `[earlydepthstencil]`).

use super::host::Host;
use super::{HostShader, lower, validator};
use crate::native::{HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource};
use crate::{io, ir, resource, structure};

/// A native program lowered as the translation does (its structure,
/// resources and stage I/O raised from its code).
fn lowered(native: &NativeProgram) -> Result<lower::Lowered, crate::ShaderError> {
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, native).expect("io::raise");
    lower::lower(&lower::Parts {
        native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: vec![],
        outputs: vec![],
        prims: vec![],
        mesh_stride: 0,
    })
}

/// A pixel program the console runs after the depth test
/// (`DEPTH_BEFORE_SHADER`) runs after it on the host too
/// (`[earlydepthstencil]`): it writes an unordered view, so without the flag
/// the host would run it, and keep its writes, for hidden pixels. Without
/// the bit it is not forced.
#[test]
fn pixel_programs_tested_before_they_run_keep_that_order() {
    let code: Vec<u32> = vec![
        0xBE84_0308, // s_mov_b32 s4, s8
        0xBE85_0380, // s_mov_b32 s5, 0
        0xF40C_0302,
        0xFA00_0000, // s_load_dwordx8 s[12:19], s[4:5], 0x0
        0x7E00_0280, // v_mov_b32 v0, 0
        0x7E02_0280, // v_mov_b32 v1, 0
        0x7E04_02F2, // v_mov_b32 v2, 1.0
        0x7E06_0280, // v_mov_b32 v3, 0
        0x7E08_0280, // v_mov_b32 v4, 0
        0x7E0A_02F2, // v_mov_b32 v5, 1.0
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0xF020_1F08,
        0x0003_0200, // image_store v[2:5], v[0:1], s[12:19] dmask:0xf dim:2D unorm
        0xF800_1890,
        0x0000_0000, // exp null off, off, off, off done vm
        0xBF81_0000, // s_endpgm
    ];
    let native = |db_shader_control| NativeProgram {
        stage: HwStage::Ps,
        wave64: true,
        vgprs: 8,
        layout: vec![RootParam {
            root: RootType::Table,
            table_offset_dwords: 0,
            sgprs: 1,
            base_register: 0,
            user_data: 8,
            slot: 0,
        }],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 8,
            user_data: 8,
            slot: 0,
        }],
        io: StageIo::Ps {
            inputs: Vec::new(),
            input_addr: 0,
            db_shader_control,
            patches: Vec::new(),
        },
        code: code.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    // DEPTH_BEFORE_SHADER, with the Z order the console pairs it with
    // (early Z then late Z, bits 4-5).
    let early = lowered(&native(1 << 12 | 1 << 4)).expect("lower");
    assert!(early.early_depth);
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut early.dxil.clone()).expect("DXIL validation failed");
    }
    let late = lowered(&native(1 << 4)).expect("lower");
    assert!(!late.early_depth);
    // The host takes the flagged program into a graphics pipeline.
    let ps = HostShader {
        stage: HwStage::Ps,
        dxil: early.dxil,
        user_data: early.user_data,
        samplers: early.samplers,
        static_samplers: Vec::new(),
        root_cbvs: Vec::new(),
        root_srvs: Vec::new(),
        root_uavs: Vec::new(),
        root_indexed_views: Vec::new(),
    };
    let vs = super::host::stub_vs(&ps).expect("vertex stub");
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        host.pipeline(&vs, &ps)
            .unwrap_or_else(|e| panic!("{label}: pipeline refused: {e}"));
    }
}
