//! `VICTORY_MERGE_MASKED=1` (`crate::masked`) end to end on the software
//! adapter, with the host's own submissions.

use super::*;

/// `memory[to] = memory[from] + add`, after `spin` steps.
const ADD: &str = r#"
#define RS "RootConstants(num32BitConstants=4, b0), UAV(u0)"
cbuffer What : register(b0) { uint to; uint from; uint add; uint spin; };
RWByteAddressBuffer memory : register(u0);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() {
    uint h = 0u;
    [loop]
    for (uint i = 0u; i < spin; i++) {
        h = (h ^ memory.Load(from)) * 0x01000193u;
    }
    memory.Store(to, memory.Load(from) + add + (h & 0u));
}
"#;

/// `VICTORY_MERGE_MASKED=1` end to end on the software adapter: two long
/// flights write a dword each of page P, one where the title writes
/// nothing, one whose first byte the title rewrites; behind them four
/// short flights elsewhere. The title rewrites every eighth byte of P
/// (8,191 changed runs, over the budget of runs) and a flight C refreshes
/// P and reads a rewritten dword of it on the card. No settle for "title
/// memory changed under shader writes": the page goes up by its mask.
/// Title memory ends with the title's bytes, the flights' writes (the
/// second one's three bytes the title did not rewrite), and what C read
/// was the title's.
///
/// `VICTORY_D3D12_WARP=1 VICTORY_CARD_BUFFERS_MIN_MB=1 VICTORY_RELAY=2
///  VICTORY_MERGE_MASKED=1 cargo test -p victory-gpu --features
///  diagnostics warp_title_bytes_under -- --test-threads=1`
#[test]
fn warp_title_bytes_under_shader_writes_go_up_by_their_masks() {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};
    const MEM_WRITE_WATCH: u32 = 0x0020_0000;
    if !crate::shadow::merge_masked() {
        eprintln!("masked tests: skipped, VICTORY_MERGE_MASKED=1 is not set");
        return;
    }
    let Some(host) = host() else {
        return;
    };
    // SAFETY: a plain query.
    let software = host
        .adapter
        .as_ref()
        .and_then(|a| unsafe { a.GetDesc1() }.ok())
        .is_some_and(|d| d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0);
    if !software || !relay() || !late_exact() {
        eprintln!("masked tests: skipped, needs the software adapter and the relay (VICTORY_D3D12_WARP=1 VICTORY_RELAY=2)");
        return;
    }
    const PIECE: usize = 64 << 10;
    const LEN: usize = 4 << 20;
    const P: usize = 5;
    // SAFETY: a plain allocation of our own.
    let title = unsafe { VirtualAlloc(std::ptr::null(), LEN, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE) } as *mut u8;
    assert!(!title.is_null());
    let dword = |at: usize| unsafe { title.add(at).cast::<u32>() };
    let old = |at: usize| 0x5A00_0000 | (at / 4) as u32;
    for i in 0..LEN / 4 {
        // SAFETY: inside the allocation.
        unsafe { dword(i * 4).write_volatile(old(i * 4)) };
    }
    crate::shadow::register(0x51_5153, title as u64, LEN as u64);
    let shadow = crate::shadow::find(title as u64, LEN as u64).expect("a host copy");
    if !shadow.carded() {
        eprintln!("masked tests: skipped, the copy is not in the card's memory (VICTORY_CARD_BUFFERS_MIN_MB=1)");
        return;
    }
    let code = compile(ADD, windows::core::s!("main"), windows::core::s!("cs_5_1")).expect("the test's shader");
    // SAFETY: the shader's own root signature; plain creation.
    let (root, pso): (ID3D12RootSignature, ID3D12PipelineState) = unsafe {
        let root: ID3D12RootSignature = host.device.CreateRootSignature(0, &code).expect("a root signature");
        let pso = host
            .device
            .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: code.as_ptr().cast(),
                    BytecodeLength: code.len(),
                },
                ..Default::default()
            })
            .expect("a pipeline");
        (root, pso)
    };
    let send = |page: usize, to: usize, from: usize, add: u32, spin: u32| {
        let list = host.new_list().expect("a host list");
        shadow.refresh(&list, title as u64, LEN as u64);
        shadow.gpu_writes(&list, (title as usize + page * PIECE) as u64, PIECE as u64);
        // SAFETY: an open list; the copy's buffer, with unordered
        // access; the shader's own root signature.
        unsafe {
            list.list.SetComputeRootSignature(&root);
            list.list.SetPipelineState(&pso);
            let constants = [to as u32, from as u32, add, spin];
            list.list.SetComputeRoot32BitConstants(0, 4, constants.as_ptr().cast(), 0);
            list.list.SetComputeRootUnorderedAccessView(1, shadow.buffer.GetGPUVirtualAddress());
            list.list.Dispatch(1, 1, 1);
        }
        list.close();
        host.execute_own(&list);
        list
    };
    let stale = LATE_STATS[WHY_STALE].load(Ordering::Relaxed);
    let masked = crate::shadow::MASKED_STATS[0].load(Ordering::Relaxed);
    // A1 and A2, long: a dword of page P each (A2's first byte is one
    // the title rewrites). Then four short flights elsewhere.
    let (a1_to, a2_to) = (P * PIECE + 256, P * PIECE + 1024);
    let mut lists = vec![send(P, a1_to, PIECE, 1, 2_000_000), send(P, a2_to, PIECE, 2, 1_000_000)];
    let b_to = |k: usize| (40 + k) * PIECE + 64;
    for k in 0..4 {
        lists.push(send(40 + k, b_to(k), 2 * PIECE, k as u32, 1_000));
    }
    let rewritten = |i: usize| (i.is_multiple_of(8) && !(256..260).contains(&i)).then_some((i / 8) as u8 ^ 0xC3);
    for i in 0..PIECE {
        if let Some(byte) = rewritten(i) {
            // SAFETY: inside the allocation.
            unsafe { title.add(P * PIECE + i).write_volatile(byte) };
        }
    }
    let c_from = P * PIECE + 512;
    let c_to = 50 * PIECE + 32;
    lists.push(send(50, c_to, c_from, 5, 1));
    settle_now(WHY_SYNC);
    assert_eq!(LATE_STATS[WHY_STALE].load(Ordering::Relaxed), stale, "the flights were settled for title memory changed under shader writes");
    assert!(crate::shadow::MASKED_STATS[0].load(Ordering::Relaxed) > masked, "no page went up by its mask");
    let mut expected: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
    for at in (P * PIECE..(P + 1) * PIECE).step_by(4) {
        let mut bytes = old(at).to_le_bytes();
        for (j, b) in bytes.iter_mut().enumerate() {
            if let Some(byte) = rewritten(at - P * PIECE + j) {
                *b = byte;
            }
        }
        expected.insert(at, u32::from_le_bytes(bytes));
    }
    expected.insert(a1_to, old(PIECE) + 1);
    // A2's dword: the title's first byte, A2's other three.
    let mut a2 = (old(PIECE) + 2).to_le_bytes();
    a2[0] = rewritten(a2_to - P * PIECE).expect("a rewritten byte");
    expected.insert(a2_to, u32::from_le_bytes(a2));
    for k in 0..4 {
        expected.insert(b_to(k), old(2 * PIECE) + k as u32);
    }
    let c_read = expected[&c_from];
    expected.insert(c_to, c_read + 5);
    for i in 0..LEN / 4 {
        let at = i * 4;
        // SAFETY: inside the allocation.
        let got = unsafe { dword(at).read_volatile() };
        let want = expected.get(&at).copied().unwrap_or_else(|| old(at));
        assert_eq!(got, want, "title memory at {at:#x}: {got:#x}, not {want:#x}");
    }
    drop(lists);
}
