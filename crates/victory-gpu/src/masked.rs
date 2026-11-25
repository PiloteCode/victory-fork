//! The pass of ours that writes the title's bytes into copies in the card's
//! memory by a byte mask (`VICTORY_MERGE_MASKED=1`, off by default; see
//! `crate::shadow::merge_masked`): its shader, its pipeline, the upload
//! memory of the masks, and its recording into the list that brings card
//! sides up to date (`Host::card_before`).
//!
//! Title bytes changed under flights' shader writes were sent as one copy a
//! changed run, at most `VICTORY_MERGE_RUNS` (512) a submission; past that
//! the flights were settled (wave 9's save gameplay: one or two full drains
//! a frame, 30 to 45 ms of the submitting thread). A page the title rewrote
//! in many runs now goes up as one dispatch: the CPU side holds the title's
//! bytes (the merge put them there), the mask says which they are, and only
//! those are written on the card, behind the flights in the queue's order.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use windows::Win32::Graphics::Direct3D12::*;

use crate::shadow::{Masked, Shadow};

/// The title's bytes into a copy's card side by their mask: a thread a dword
/// from dword `first` on, `dwords` of them; the mask is a bit per byte (four
/// per dword, dword `j`'s in bits `4 (j % 8)` of mask word `j / 8`, from mask
/// word `mask_at`). A dword with all four bits takes the CPU side's dword;
/// one with some takes those bytes and keeps the card's others; one with
/// none is not touched.
pub(crate) const CARD_MASKED: &str = r#"
#define RS "RootConstants(num32BitConstants=3, b0), SRV(t0), SRV(t1), UAV(u0)"
cbuffer What : register(b0) {
    uint first;    // the first dword of the copy
    uint dwords;
    uint mask_at;  // the first mask word
};
ByteAddressBuffer cpu : register(t0);
ByteAddressBuffer masks : register(t1);
RWByteAddressBuffer card : register(u0);
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint j = id.x;
    if (j >= dwords) return;
    uint four = (masks.Load((mask_at + (j >> 3)) * 4u) >> ((j & 7u) * 4u)) & 0xFu;
    if (four == 0u) return;
    uint at = (first + j) * 4u;
    uint mine = cpu.Load(at);
    if (four == 0xFu) {
        card.Store(at, mine);
        return;
    }
    uint m = ((four & 1u) != 0u ? 0x000000FFu : 0u) | ((four & 2u) != 0u ? 0x0000FF00u : 0u)
        | ((four & 4u) != 0u ? 0x00FF0000u : 0u) | ((four & 8u) != 0u ? 0xFF000000u : 0u);
    card.Store(at, (card.Load(at) & ~m) | (mine & m));
}
"#;

/// The groups of [`CARD_MASKED`] for a mask of `dwords` dwords.
pub(crate) fn groups(dwords: usize) -> u32 {
    dwords.div_ceil(64).clamp(1, 65535) as u32
}

/// Upload memory for masks: buffer, mapping, bytes.
pub(crate) type Buffer = (ID3D12Resource, usize, u64);

/// What the host keeps for the pass, with its other work for copies in the
/// card's memory.
pub(crate) struct Pass {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
    /// Upload buffers for the masks not in use: a plan takes one ...
    spare: Vec<Buffer>,
    /// ... held here by the plan's number in the queue's order until the
    /// plan is done (`Host::card_done`).
    out: Vec<(u64, Buffer)>,
}

impl Pass {
    /// The pass, with the switch on; `None` (and pages refused as before)
    /// if it is off or cannot be made.
    pub(crate) fn make(device: &ID3D12Device) -> Option<Pass> {
        if !crate::shadow::merge_masked() {
            return None;
        }
        let made = (|| -> windows::core::Result<Pass> {
            let code = crate::host::compile(CARD_MASKED, windows::core::s!("main"), windows::core::s!("cs_5_1"))?;
            // SAFETY: the blob's own bytes; the root signature is inside the
            // shader.
            unsafe {
                let root: ID3D12RootSignature = device.CreateRootSignature(0, &code)?;
                let pso: ID3D12PipelineState = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: code.as_ptr().cast(),
                        BytecodeLength: code.len(),
                    },
                    ..Default::default()
                })?;
                Ok(Pass {
                    root,
                    pso,
                    spare: Vec::new(),
                    out: Vec::new(),
                })
            }
        })();
        crate::shadow::masked_ready(made.is_ok());
        match made {
            Ok(pass) => {
                tracing::info!(
                    "title bytes under flights' shader writes go up by their byte masks when a page holds many changed runs \
                     (VICTORY_MERGE_MASKED=1)"
                );
                Some(pass)
            }
            Err(e) => {
                tracing::warn!(error = %e, "no masked pass (VICTORY_MERGE_MASKED=1): such pages settle the flights as before");
                None
            }
        }
    }
}

/// The copies with masks to send join those the list brings up to date
/// (with no ranges of their own if they have none).
pub(crate) fn join(dirty: &mut Vec<(Arc<Shadow>, Vec<(usize, usize)>)>, masked: &[(Arc<Shadow>, Vec<Masked>)]) {
    for (shadow, _) in masked {
        if !dirty.iter().any(|(s, _)| Arc::ptr_eq(s, shadow)) {
            dirty.push((shadow.clone(), Vec::new()));
        }
    }
}

/// The upload buffer for a plan's masks, if it has any: a spare one big
/// enough, else a new one. Fails the run if there is no pass or no memory
/// (the masks' bytes are in the CPU sides only, and must go up).
pub(crate) fn take(pass: Option<&mut Pass>, masked: &[(Arc<Shadow>, Vec<Masked>)], host: &crate::host::Host) -> Option<Buffer> {
    let words: usize = masked.iter().flat_map(|(_, m)| m.iter().map(|m| m.bits.len())).sum();
    if words == 0 {
        return None;
    }
    let Some(pass) = pass else {
        crate::host::fail(format_args!("title bytes to go up by their masks, and no masked pass (VICTORY_MERGE_MASKED=1)"));
    };
    let bytes = (words * 4) as u64;
    if let Some(at) = pass.spare.iter().position(|m| m.2 >= bytes) {
        return Some(pass.spare.swap_remove(at));
    }
    let size = bytes.next_power_of_two().max(64 << 10);
    let Some((buffer, data)) = host.upload_buffer(size) else {
        crate::host::fail(format_args!("no upload memory for {bytes} bytes of masks (VICTORY_MERGE_MASKED=1)"));
    };
    crate::host::name(&buffer, || "title bytes' masks for card copies".into());
    Some((buffer, data as usize, size))
}

/// The plan numbered `sent_as` holds `buffer` until it is done.
pub(crate) fn hold(pass: Option<&mut Pass>, sent_as: u64, buffer: Option<Buffer>) {
    if let (Some(pass), Some(buffer)) = (pass, buffer) {
        pass.out.push((sent_as, buffer));
    }
}

/// The plan numbered `sent_as` is done: its buffer is spare again.
pub(crate) fn done(pass: Option<&mut Pass>, sent_as: u64) {
    let Some(pass) = pass else {
        return;
    };
    while let Some(at) = pass.out.iter().position(|m| m.0 == sent_as) {
        let (_, buffer) = pass.out.swap_remove(at);
        pass.spare.push(buffer);
    }
}

/// The masks of `shadow` among `masked`.
pub(crate) fn of<'a>(masked: &'a [(Arc<Shadow>, Vec<Masked>)], shadow: &Arc<Shadow>) -> &'a [Masked] {
    masked.iter().find(|(s, _)| Arc::ptr_eq(s, shadow)).map_or(&[], |(_, m)| &m[..])
}

/// Records the pass over `mine` (the masks of `shadow`, whose CPU side is
/// `staging`) into `list`, after the copies of its changed ranges (`copied`:
/// whether there were any, the buffer is then in the copy state), with the
/// pass and the plan's masks' buffer (`with`), the masks' words written
/// into it from word `*mask_at` on. Leaves the card side readable by
/// shaders, for the hash: whether it recorded.
///
/// # Safety
/// `list` is open; the buffers are live and hold what is addressed.
pub(crate) unsafe fn record(
    list: &ID3D12GraphicsCommandList,
    shadow: &Shadow,
    staging: &ID3D12Resource,
    copied: bool,
    mine: &[Masked],
    with: (Option<&Pass>, Option<&Buffer>),
    mask_at: &mut usize,
) -> bool {
    let (Some(pass), Some((buffer, data, _))) = with else {
        return false;
    };
    if mine.is_empty() {
        return false;
    }
    let was = if copied { D3D12_RESOURCE_STATE_COPY_DEST } else { D3D12_RESOURCE_STATE_COMMON };
    // SAFETY: the caller's.
    unsafe {
        list.ResourceBarrier(&[crate::host::transition(&shadow.buffer, was, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)]);
        list.SetComputeRootSignature(&pass.root);
        list.SetPipelineState(&pass.pso);
        list.SetComputeRootShaderResourceView(1, staging.GetGPUVirtualAddress());
        list.SetComputeRootShaderResourceView(2, buffer.GetGPUVirtualAddress());
        list.SetComputeRootUnorderedAccessView(3, shadow.buffer.GetGPUVirtualAddress());
        for m in mine {
            // The plan's own buffer, sized for all its masks; the card reads
            // it when the list runs.
            std::ptr::copy_nonoverlapping(m.bits.as_ptr(), (*data as *mut u32).add(*mask_at), m.bits.len());
            let constants = [(m.base / 4) as u32, m.dwords as u32, *mask_at as u32];
            list.SetComputeRoot32BitConstants(0, 3, constants.as_ptr().cast(), 0);
            list.Dispatch(groups(m.dwords), 1, 1);
            *mask_at += m.bits.len();
        }
        list.ResourceBarrier(&[crate::host::transition(
            &shadow.buffer,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
        )]);
    }
    #[cfg(feature = "diagnostics")]
    crate::barrier::ours(crate::barrier::Ours::CardUpload, 2);
    true
}

/// The pieces to hash again after the pass: those of each mask.
pub(crate) fn runs<'a>(shadow: &'a Shadow, mine: &'a [Masked]) -> impl Iterator<Item = crate::shadow::CardSpan> + 'a {
    mine.iter().map(|m| shadow.card_run(m.base, m.base + 4 * m.dwords))
}

/// With the frame line.
pub(crate) fn line(frames: f64) {
    if !crate::shadow::merge_masked() {
        return;
    }
    let [pages, bytes, sent, hidden] = [0, 1, 2, 3].map(|i| crate::shadow::MASKED_STATS[i].swap(0, Ordering::Relaxed) as f64 / frames);
    tracing::info!(
        "title bytes under flights' shader writes by byte masks per frame (VICTORY_MERGE_MASKED=1): {pages:.1} pages \
         ({:.3} MB changed) went up by their masks, {sent:.1} masks sent, {hidden:.1} merges of fetched pieces left \
         masked bytes out",
        bytes / (1 << 20) as f64
    );
}
