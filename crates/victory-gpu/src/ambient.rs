//! A constant indirect diffuse light, on asking (`VICTORY_AMBIENT`).
//!
//! The title's indirect diffuse light comes from screen probes it fills by
//! tracing rays. Where the ray passes do not run (their structure builder
//! is off, or their programs do not translate), the program that resolves
//! the probes (by default the one whose name starts with
//! `CS_ScreenProbes_TemporalResolve`) leaves whatever its inputs' memory
//! held: on Belobug's fork, bright magenta on leaves, cloth and paper, and
//! blocks of colours indoors. Right after that program, its output (R9G9B9E5
//! packed into a 32-bit integer texture) is replaced here by one colour,
//! the same everywhere; the title's lighting then applies it with the
//! surfaces' colours. Replaced, not raised to a floor: what the program
//! wrote is garbage then, and the result is also the next frame's history.
//!
//! Taken from Belobug's fork (06031237, 4f45ab88, 4d63a091), where it is on
//! at 0.6. Here it is off unless asked for: the ray passes run here in part
//! and give rooms their light (skipping them left rooms black), and a
//! constant would replace what they give. `VICTORY_AMBIENT=<level>` turns it
//! on (0.6 looked balanced on that fork; 0 is no indirect diffuse light at
//! all). Once the probes are traced, leave it off.
//! `VICTORY_AMBIENT_PROGRAM=<name start>` and `VICTORY_AMBIENT_TABLE=<root
//! slot>,<dword>` name another program, or another place of its output in
//! its tables (by default slot 1, dword 176).
//!
//! shortcut: the same light everywhere, from everywhere.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::s;

use crate::host::{HostList, host};
use crate::object::This;

/// The level asked for (`VICTORY_AMBIENT`); `None`: off (the default).
pub(crate) fn level() -> Option<f32> {
    static LEVEL: OnceLock<Option<f32>> = OnceLock::new();
    *LEVEL.get_or_init(|| {
        let set = std::env::var("VICTORY_AMBIENT").ok()?;
        let level = set.trim().parse::<f32>().ok().filter(|l| l.is_finite() && *l >= 0.0);
        match level {
            Some(level) => tracing::info!(level, "the probes' indirect diffuse light is replaced by a constant (VICTORY_AMBIENT)"),
            None if set.trim() != "off" => tracing::warn!(set, "VICTORY_AMBIENT is not a level: off"),
            None => {}
        }
        level
    })
}

/// The start of the name of the program whose output is replaced
/// (`VICTORY_AMBIENT_PROGRAM`).
fn program() -> &'static [u8] {
    static NAME: OnceLock<Vec<u8>> = OnceLock::new();
    NAME.get_or_init(|| {
        std::env::var("VICTORY_AMBIENT_PROGRAM")
            .ok()
            .map(|n| n.trim().as_bytes().to_vec())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| b"CS_ScreenProbes_TemporalResolve".to_vec())
    })
}

/// Whether a shader container is the program whose output is replaced: its
/// name is in it. Always false while [`level`] is off.
pub(crate) fn resolves_light(container: &[u8]) -> bool {
    if level().is_none() {
        return false;
    }
    let name = program();
    container.windows(name.len()).any(|w| w == name)
}

/// Where the program's output lies in its tables: the title's root slot
/// and the dword in the table (`VICTORY_AMBIENT_TABLE=<slot>,<dword>`).
pub(crate) fn place() -> (u8, u32) {
    static PLACE: OnceLock<(u8, u32)> = OnceLock::new();
    *PLACE.get_or_init(|| {
        std::env::var("VICTORY_AMBIENT_TABLE")
            .ok()
            .and_then(|v| {
                let (slot, dword) = v.split_once(',')?;
                Some((slot.trim().parse().ok()?, dword.trim().parse().ok()?))
            })
            .unwrap_or((1, 176))
    })
}

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=4, b0), DescriptorTable(UAV(u0))"
cbuffer Light : register(b0) { float level; float3 tint; };
RWTexture2D<uint> light : register(u0);
uint pack(float3 c) {
    c = clamp(c, 0.0, 65408.0);
    float most = max(c.r, max(c.g, c.b));
    int e = clamp(int(floor(log2(max(most, 1e-9)))) + 16, 0, 31);
    float scale = exp2(float(24 - e));
    if (most * scale >= 511.5) { e = min(e + 1, 31); scale = exp2(float(24 - e)); }
    uint3 m = min(uint3(c * scale + 0.5), 511u);
    return m.r | (m.g << 9) | (m.b << 18) | (uint(e) << 27);
}
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 at : SV_DispatchThreadID) {
    uint w, h;
    light.GetDimensions(w, h);
    if (at.x >= w || at.y >= h) return;
    light[at.xy] = pack(tint * level);
}
"#;

/// Our views of the output, used in turn: more than the lists in flight
/// hold passes of this.
const SLOTS: u32 = 1024;

struct Pass {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
    views: ID3D12DescriptorHeap,
    step: u32,
    next: AtomicU32,
}

// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Pass {}
// SAFETY: as above.
unsafe impl Sync for Pass {}

fn make(device: &ID3D12Device) -> windows::core::Result<Pass> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(
            SOURCE.as_ptr().cast(),
            SOURCE.len(),
            s!("ambient"),
            None,
            None,
            s!("main"),
            s!("cs_5_1"),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    let bytes = |b: &ID3DBlob| {
        // SAFETY: the blob's own bytes.
        unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }.to_vec()
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            tracing::warn!("ambient light: {}", String::from_utf8_lossy(&bytes(&errors)));
        }
        return Err(e);
    }
    let code = bytes(&code.ok_or_else(windows::core::Error::empty)?);
    // SAFETY: the root signature is inside the shader; plain creations.
    unsafe {
        let root: ID3D12RootSignature = device.CreateRootSignature(0, &code)?;
        let pso = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: code.as_ptr().cast(),
                BytecodeLength: code.len(),
            },
            ..Default::default()
        })?;
        let kind = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV;
        let views = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
            Type: kind,
            NumDescriptors: SLOTS,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
            NodeMask: 0,
        })?;
        Ok(Pass {
            root,
            pso,
            views,
            step: device.GetDescriptorHandleIncrementSize(kind),
            next: AtomicU32::new(0),
        })
    }
}

fn pass() -> Option<&'static Pass> {
    static PASS: OnceLock<Option<Pass>> = OnceLock::new();
    PASS.get_or_init(|| {
        make(&host()?.device)
            .inspect_err(|e| tracing::warn!(error = %e, "no ambient light pass"))
            .ok()
    })
    .as_ref()
}

fn uav_barrier(list: &ID3D12GraphicsCommandList) {
    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        ..Default::default()
    };
    // SAFETY: an open list; the barrier names no resource.
    unsafe { list.ResourceBarrier(&[barrier]) };
}

/// Replaces what title texture `light` holds by the constant ([`level`]),
/// in `list`, right after the program that wrote it through unordered
/// access (it is in that state). Whether anything was recorded: the list's
/// descriptor heaps, compute root signature and pipeline are ours then.
pub(crate) fn replace(list: &HostList, light: This) -> bool {
    let Some(level) = level() else {
        return false;
    };
    let (Some(host), Some(pass)) = (host(), pass()) else {
        return false;
    };
    let Some(texture) = crate::resource::host_resource(light) else {
        return false;
    };
    // SAFETY: a live resource.
    let d = unsafe { texture.GetDesc() };
    if d.Format != DXGI_FORMAT_R32_UINT || d.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::warn!(format = d.Format.0, "no ambient light: the output is not a 32-bit integer image"));
        return false;
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| tracing::info!(level, width = d.Width, height = d.Height, "ambient light replaces the probes' output"));
    let slot = pass.next.fetch_add(1, Ordering::Relaxed) % SLOTS;
    list.keep(texture);
    list.keep(&pass.views);
    let l = &list.list;
    uav_barrier(l);
    // SAFETY: an open list; a live texture the list keeps, in the
    // unordered-access state the program before left it in; a view in a
    // slot no list in flight reads.
    unsafe {
        let cpu = pass.views.GetCPUDescriptorHandleForHeapStart();
        let gpu = pass.views.GetGPUDescriptorHandleForHeapStart();
        let at = D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: cpu.ptr + (slot * pass.step) as usize,
        };
        let view = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_UINT,
            ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        host.device.CreateUnorderedAccessView(texture, None::<&ID3D12Resource>, Some(&view), at);
        l.SetDescriptorHeaps(&[Some(pass.views.clone())]);
        l.SetComputeRootSignature(&pass.root);
        l.SetPipelineState(&pass.pso);
        // A slightly warm white, as on that fork.
        let constants = [level, 1.0f32, 0.98, 0.94];
        l.SetComputeRoot32BitConstants(0, 4, constants.as_ptr().cast(), 0);
        l.SetComputeRootDescriptorTable(
            1,
            D3D12_GPU_DESCRIPTOR_HANDLE {
                ptr: gpu.ptr + u64::from(slot * pass.step),
            },
        );
        l.Dispatch((d.Width as u32).div_ceil(8), d.Height.div_ceil(8), 1);
    }
    uav_barrier(l);
    true
}

#[cfg(test)]
mod tests {
    /// The pass compiles with the system's compiler and the host takes it.
    #[test]
    fn ambient_pass_builds() {
        let Some(host) = crate::host::host() else {
            return;
        };
        super::make(&host.device).expect("the ambient pass");
    }

    /// Off unless asked for: no container is the program then.
    #[test]
    fn off_by_default() {
        if std::env::var_os("VICTORY_AMBIENT").is_some() {
            return;
        }
        assert!(super::level().is_none());
        assert!(!super::resolves_light(b"..CS_ScreenProbes_TemporalResolve_Main.."));
    }
}
