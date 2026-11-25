//! The card's own memory mapped for the processor (Direct3D 12 GPU upload
//! heaps: video memory the processor writes through a resizable BAR), as a
//! home for memory the processor writes and the card reads
//! (`VICTORY_GPU_UPLOAD_HEAP`, off by default).
//!
//! The question behind it: the console has one memory, so what the
//! title's processor writes the card reads where it lies; here every such
//! byte goes through a copy (the write watch or a guard finds it, the
//! submitting thread copies it into a host copy, the card's side is
//! brought up to date before the lists run), and what shaders write comes
//! back the same way. Memory of the card's that the processor maps could
//! hold title memory itself, with nothing to copy either way and nothing
//! to guard. What stands in the way, and what this module measures:
//!
//! - Where the processor sees such memory is the kernel's choice (`Map`
//!   takes no address), while the title places its pages itself
//!   (`XMemMapPhysicalPages` into placeholders it reserved, some pages at
//!   several addresses at once), and its busiest buffer (174 MB of
//!   per-draw constants and shader data, behind every settle of the
//!   frame) lies below 4 GiB. The probe says where a mapping lands, also
//!   when room is made for one below 4 GiB.
//! - The processor reads the card's memory uncached, over the bus: what
//!   the title or this layer reads has to stay in the PC's memory. The
//!   probe measures both directions; the tally
//!   (`VICTORY_GPU_UPLOAD_TALLY=1`) counts, per title reservation, what
//!   this layer copies and reads of it and how the title maps it.
//!
//! What `VICTORY_GPU_UPLOAD_HEAP=1` changes, a first step that the
//! processor's side never reads back: the lists' scratch memory (constants
//! filled in for tables and root views, rectangles' vertices) and the
//! texture staging buffers are made in the card's memory, so that the card
//! reads them there instead of over the bus. Without the device's support
//! (`D3D12_FEATURE_D3D12_OPTIONS16`), or if the kernel's copy of title
//! memory cannot write into such memory, or when the card refuses one,
//! they are made in upload memory as before. `=probe`: the probe alone.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{OnceLock, RwLock};
use std::time::Instant;

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows_sys::Win32::System::Memory as mem;

/// `VICTORY_GPU_UPLOAD_HEAP`: 0 (unset) off; 1 the buffers the processor
/// only writes in the card's memory, and the probe; 2 (`probe`) the probe
/// alone.
pub(crate) fn level() -> u8 {
    static LEVEL: OnceLock<u8> = OnceLock::new();
    *LEVEL.get_or_init(|| parse_level(std::env::var("VICTORY_GPU_UPLOAD_HEAP").ok().as_deref()))
}

fn parse_level(value: Option<&str>) -> u8 {
    match value.map(str::trim) {
        Some("1") => 1,
        Some("probe") => 2,
        _ => 0,
    }
}

/// `VICTORY_GPU_UPLOAD_TALLY=1`: the tally of title reservations (logging
/// only; [`reserved`]).
pub(crate) fn tally_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_GPU_UPLOAD_TALLY").is_some_and(|v| v == "1"))
}

/// What the device offers, found when it is made ([`device_ready`]).
#[derive(Clone, Copy, Default, Debug)]
struct Support {
    /// `GPUUploadHeapSupported` of `D3D12_FEATURE_D3D12_OPTIONS16`.
    heap: bool,
    /// The kernel's copy of title memory (`crate::host::read_into`, which
    /// fills scratch memory) writes into such memory and the bytes are
    /// there (asked only with level 1).
    kernel_copy: bool,
}

static SUPPORT: OnceLock<Support> = OnceLock::new();

/// Whether `device` has GPU upload heaps.
pub(crate) fn heaps_supported(device: &ID3D12Device) -> bool {
    let mut o16 = D3D12_FEATURE_DATA_D3D12_OPTIONS16::default();
    // SAFETY: a plain query that fills our structure.
    unsafe { device.CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS16, (&raw mut o16).cast(), size_of_val(&o16) as u32) }
        .is_ok()
        && o16.GPUUploadHeapSupported.as_bool()
}

/// The SDK version of the Direct3D 12 runtime this process loaded (the
/// `D3D12SDKVersion` its `D3D12Core.dll` exports: the system's own unless
/// the program asks for another), and the one the program asks for (an
/// export of the same name in the executable; `None`: it asks for none and
/// gets the system's).
fn sdk_versions() -> (Option<u32>, Option<u32>) {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    let read = |module: windows_sys::Win32::Foundation::HMODULE| -> Option<u32> {
        if module.is_null() {
            return None;
        }
        // SAFETY: a plain lookup in a loaded module; the export is a
        // 32-bit number in its data.
        unsafe {
            let at = GetProcAddress(module, c"D3D12SDKVersion".as_ptr().cast())?;
            Some((at as usize as *const u32).read_unaligned())
        }
    };
    let core: Vec<u16> = "D3D12Core.dll\0".encode_utf16().collect();
    // SAFETY: plain module lookups; nothing is loaded or freed.
    unsafe { (read(GetModuleHandleW(core.as_ptr())), read(GetModuleHandleW(std::ptr::null()))) }
}

/// The device was made: what it offers is found and said, and with the
/// switch the probe starts on a thread of its own.
pub(crate) fn device_ready(device: &ID3D12Device) {
    let heap = heaps_supported(device);
    let kernel_copy = heap && level() == 1 && kernel_copy_works(device);
    let support = Support { heap, kernel_copy };
    let _ = SUPPORT.set(support);
    let (runtime, asked) = sdk_versions();
    let sdk = |v: Option<u32>| v.map_or_else(|| "none".to_owned(), |v| v.to_string());
    tracing::info!(
        "GPU upload heaps (the card's memory mapped for the processor, through a resizable BAR): {} by the device \
         (OPTIONS16); the Direct3D 12 runtime loaded is SDK version {} (the program asks for {}: {}); \
         VICTORY_GPU_UPLOAD_HEAP={}{}",
        if heap { "supported" } else { "NOT supported" },
        sdk(runtime),
        sdk(asked),
        if asked.is_some() { "its own runtime" } else { "the system's" },
        match level() {
            1 => "1: scratch memory and texture staging in the card's memory",
            2 => "probe: the probe alone",
            _ => "0 (off)",
        },
        match (level(), heap, kernel_copy) {
            (1, true, false) => " - NOT used: the kernel's copy of title memory into such memory failed",
            (1, false, _) => " - NOT used: no support",
            _ => "",
        }
    );
    if level() > 0 && heap {
        let spawned = std::thread::Builder::new().name("gpu upload heap probe".into()).spawn(|| {
            // Waits for the host to be made (the caller is making it).
            let Some(host) = crate::host::host() else {
                return;
            };
            let started = Instant::now();
            let lines = probe(&host.device, PROBE_MB, LOW_ROOM.get().copied());
            for line in &lines {
                tracing::info!("GPU upload heap probe: {line}");
            }
            tracing::info!("GPU upload heap probe: done in {:.0} ms", started.elapsed().as_secs_f64() * 1e3);
        });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "no thread for the GPU upload heap probe");
        }
    }
}

/// Whether buffers the processor only writes are made in the card's memory
/// ([`crate::host::Host::written_buffer`]).
pub(crate) fn buffers() -> bool {
    level() == 1 && SUPPORT.get().is_some_and(|s| s.heap && s.kernel_copy)
}

/// The heap properties of a GPU upload heap.
pub(crate) fn heap_properties() -> D3D12_HEAP_PROPERTIES {
    D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_GPU_UPLOAD, ..Default::default() }
}

pub(crate) fn buffer_desc(size: u64) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size.max(1),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    }
}

/// A buffer of `size` bytes in a GPU upload heap, in the common state, and
/// its mapping (write-combined: write it, never read it).
pub(crate) fn make(device: &ID3D12Device, size: u64) -> windows::core::Result<(ID3D12Resource, *mut u8)> {
    let mut buffer: Option<ID3D12Resource> = None;
    let mut data = std::ptr::null_mut();
    // SAFETY: plain creation and mapping of a buffer of our own.
    unsafe {
        device.CreateCommittedResource(
            &heap_properties(),
            D3D12_HEAP_FLAG_NONE,
            &buffer_desc(size),
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut buffer,
        )?;
        let buffer = buffer.ok_or_else(windows::core::Error::empty)?;
        buffer.Map(0, None, Some(&mut data))?;
        Ok((buffer, data.cast()))
    }
}

/// Buffers made in the card's memory for [`buffers`] and their bytes;
/// buffers asked for there that the card refused (made in upload memory
/// instead) and their bytes; since the start.
static MADE: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// A buffer of `size` bytes was made in the card's memory for [`buffers`].
pub(crate) fn made(size: u64) {
    MADE[0].fetch_add(1, Relaxed);
    MADE[1].fetch_add(size, Relaxed);
}

/// The card refused a buffer of `size` bytes ([`buffers`]): it is made in
/// upload memory. The first is said.
pub(crate) fn refused(size: u64, error: &windows::core::Error) {
    if MADE[2].fetch_add(1, Relaxed) == 0 {
        tracing::warn!(size, %error, "no buffer in a GPU upload heap: made in upload memory (said once)");
    }
    MADE[3].fetch_add(size, Relaxed);
}

/// Whether the kernel's copy of title memory (`crate::host::read_into`,
/// as fills of scratch memory make it) writes into memory of a GPU upload
/// heap, in the sizes fills take, and the bytes are there afterwards (read
/// back once, through the mapping: slow, but 64 KiB).
fn kernel_copy_works(device: &ID3D12Device) -> bool {
    const SIZE: usize = 64 << 10;
    let (buffer, data) = match make(device, SIZE as u64) {
        Ok(made) => made,
        Err(e) => {
            tracing::warn!(error = %e, "no buffer in a GPU upload heap to try the kernel's copy with");
            return false;
        }
    };
    let source: Vec<u8> = (0..SIZE).map(|i| (i as u8).wrapping_mul(29).wrapping_add(11)).collect();
    // SAFETY: the mapping holds `SIZE` bytes while `buffer` lives.
    let target = unsafe { std::slice::from_raw_parts_mut(data, SIZE) };
    // A fill's size, a page and the rest, at the offsets fills land on.
    let copied = crate::host::read_into(source.as_ptr(), &mut target[..480])
        && crate::host::read_into(source[480..].as_ptr(), &mut target[480..480 + 4096])
        && crate::host::read_into(source[4576..].as_ptr(), &mut target[4576..]);
    // The write-combined stores are out before they are read back.
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    let mut back = vec![0u8; SIZE];
    // SAFETY: as above; the processor reads the card's memory here.
    unsafe { std::ptr::copy_nonoverlapping(data.cast_const(), back.as_mut_ptr(), SIZE) };
    drop(buffer);
    copied && back == source
}

/// The bytes of the probe's buffers (MiB).
const PROBE_MB: usize = 64;

/// Makes room below 4 GiB for `bytes` and calls `place` there once (it
/// maps something, notes where that landed, undoes it and returns the
/// address, 0 for none): the runtime's guards over that space give way
/// for the call and are put back after it. Given by the runtime
/// ([`set_low_room`]); 0 if no guard is that large.
pub type LowRoom = fn(usize, &mut dyn FnMut() -> usize) -> usize;

static LOW_ROOM: OnceLock<LowRoom> = OnceLock::new();

/// The runtime's way to make room below 4 GiB, for the probe: whether the
/// card's memory, mapped for the processor, can land where the title's
/// 32-bit memory has to be.
pub fn set_low_room(room: LowRoom) {
    let _ = LOW_ROOM.set(room);
}

fn gbps(bytes: usize, seconds: f64) -> f64 {
    bytes as f64 / seconds.max(1e-9) / 1e9
}

/// The shortest of three runs of `f`, in seconds.
fn best_of_three(mut f: impl FnMut()) -> f64 {
    (0..3)
        .map(|_| {
            let started = Instant::now();
            f();
            started.elapsed().as_secs_f64()
        })
        .fold(f64::MAX, f64::min)
}

/// Copies `source` to `to` whole, then as the fills of scratch memory do
/// (480 bytes at every 512): the seconds of each, the best of three.
fn write_speeds(to: *mut u8, source: &[u8]) -> (f64, f64) {
    let whole = best_of_three(|| {
        // SAFETY: `to` holds `source.len()` bytes (the caller's mapping).
        unsafe { std::ptr::copy_nonoverlapping(source.as_ptr(), to, source.len()) }
    });
    let fills = best_of_three(|| {
        for at in (0..source.len().saturating_sub(512)).step_by(512) {
            // SAFETY: as above, 480 bytes from `at` lie inside both.
            unsafe { std::ptr::copy_nonoverlapping(source.as_ptr().add(at), to.add(at), 480) }
        }
    });
    (whole, fills)
}

/// Reads `len` bytes at `from` with plain loads (8 bytes each), as code
/// that reads memory does; their sum, so that nothing is left out.
fn read_plain(from: *const u8, len: usize) -> u64 {
    let mut sum = 0u64;
    for i in 0..len / 8 {
        // SAFETY: the caller's mapping holds `len` bytes.
        sum = sum.wrapping_add(unsafe { from.cast::<u64>().add(i).read_volatile() });
    }
    sum
}

/// Reads `len` bytes at `from` with streaming loads (`movntdqa`, the way
/// write-combined memory reads fastest).
///
/// # Safety
/// `from` holds `len` bytes and is 16-byte aligned; the processor has
/// SSE4.1.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn read_streaming(from: *const u8, len: usize) -> u64 {
    use std::arch::x86_64::{__m128i, _mm_setzero_si128, _mm_storeu_si128, _mm_stream_load_si128, _mm_xor_si128};
    let mut acc = _mm_setzero_si128();
    for i in 0..len / 16 {
        // SAFETY: inside the caller's mapping, aligned.
        acc = _mm_xor_si128(acc, unsafe { _mm_stream_load_si128(from.cast::<__m128i>().add(i).cast_mut()) });
    }
    let mut out = [0u64; 2];
    // SAFETY: 16 bytes of our own.
    unsafe { _mm_storeu_si128(out.as_mut_ptr().cast(), acc) };
    out[0] ^ out[1]
}

/// Seconds to read `len` bytes at `from`, plain and streaming (`None`
/// without SSE4.1).
fn read_speeds(from: *const u8, len: usize) -> (f64, Option<f64>) {
    let plain = best_of_three(|| {
        std::hint::black_box(read_plain(from, len));
    });
    #[cfg(target_arch = "x86_64")]
    let streaming = std::arch::is_x86_feature_detected!("sse4.1").then(|| {
        best_of_three(|| {
            // SAFETY: mappings are 64 KiB aligned and hold `len` bytes.
            std::hint::black_box(unsafe { read_streaming(from, len) });
        })
    });
    #[cfg(not(target_arch = "x86_64"))]
    let streaming = None;
    (plain, streaming)
}

/// A buffer of `size` bytes in a custom heap of the PC's memory, write-back
/// (as `Host::shared_buffer` makes host copies of title memory), mapped.
fn write_back(device: &ID3D12Device, size: u64) -> windows::core::Result<(ID3D12Resource, *mut u8)> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_CUSTOM,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
        MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
        ..Default::default()
    };
    let mut buffer: Option<ID3D12Resource> = None;
    let mut data = std::ptr::null_mut();
    // SAFETY: plain creation and mapping of a buffer of our own.
    unsafe {
        device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &buffer_desc(size), D3D12_RESOURCE_STATE_COMMON, None, &mut buffer)?;
        let buffer = buffer.ok_or_else(windows::core::Error::empty)?;
        buffer.Map(0, None, Some(&mut data))?;
        Ok((buffer, data.cast()))
    }
}

/// Maps `resource` (an upload buffer) for writing.
fn mapped(resource: &ID3D12Resource) -> windows::core::Result<*mut u8> {
    let mut data = std::ptr::null_mut();
    // SAFETY: a mappable buffer of our own.
    unsafe { resource.Map(0, None, Some(&mut data))? };
    Ok(data.cast())
}

/// Where `[at, at + bytes)` lies against the lines title memory must keep:
/// 4 GiB (the title's 32-bit memory, its 174 MB buffer among it), 128 GiB
/// (the directory through which shaders reach memory by address,
/// `crate::memory`) and 1 TiB (the console's 40 bits, which the runtime
/// keeps the title's other memory under).
fn placement(at: usize, bytes: usize) -> String {
    let end = at.saturating_add(bytes);
    let side = |line: usize| if end <= line { "below" } else { "above" };
    format!("{} 4 GiB, {} 128 GiB, {} 1 TiB", side(1 << 32), side(1 << 37), side(1 << 40))
}

/// What `VirtualQuery` says of `at`.
fn query(at: *const u8) -> String {
    // SAFETY: plain data filled in by the query.
    let mut m: mem::MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    if unsafe { mem::VirtualQuery(at.cast(), &mut m, size_of_val(&m)) } == 0 {
        return format!("VirtualQuery failed ({})", std::io::Error::last_os_error());
    }
    format!(
        "type {:#x} (0x20000 private, 0x40000 mapped, 0x1000000 image), state {:#x}, protection {:#x} (0x4 read-write, 0x404 write-combined), \
         allocation base {:#x} (the mapping {:#x}), region {} KiB",
        m.Type,
        m.State,
        m.Protect,
        m.AllocationBase as usize,
        at as usize,
        m.RegionSize >> 10
    )
}

/// Milliseconds the card takes to copy `bytes` from `from` into `to`, by
/// its own clock (timestamps around the copy), the best of three.
fn copy_gpu_ms(rig: &mut crate::inplace::Rig, to: &ID3D12Resource, from: &ID3D12Resource, bytes: u64) -> windows::core::Result<f64> {
    let mut heap: Option<ID3D12QueryHeap> = None;
    // SAFETY: plain creation.
    unsafe {
        rig.device().CreateQueryHeap(
            &D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: 2, NodeMask: 0 },
            &mut heap,
        )?
    };
    let heap = heap.ok_or_else(windows::core::Error::empty)?;
    let back = rig.committed(D3D12_HEAP_TYPE_READBACK, 16, D3D12_RESOURCE_STATE_COPY_DEST)?;
    // SAFETY: a plain query of the rig's queue.
    let frequency = unsafe { rig.queue().GetTimestampFrequency()? }.max(1) as f64;
    let mut best = f64::MAX;
    for _ in 0..3 {
        // SAFETY: live buffers of `bytes` bytes or more, our query heap.
        rig.run(|list| unsafe {
            list.EndQuery(&heap, D3D12_QUERY_TYPE_TIMESTAMP, 0);
            list.CopyBufferRegion(to, 0, from, 0, bytes);
            list.EndQuery(&heap, D3D12_QUERY_TYPE_TIMESTAMP, 1);
            list.ResolveQueryData(&heap, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &back, 0);
        })?;
        let mut data = std::ptr::null_mut();
        // SAFETY: the read-back buffer holds the two timestamps.
        let [start, end] = unsafe {
            back.Map(0, None, Some(&mut data))?;
            let ticks = data.cast::<[u64; 2]>().read();
            back.Unmap(0, None);
            ticks
        };
        best = best.min(end.saturating_sub(start) as f64 / frequency * 1e3);
    }
    Ok(best)
}

/// The probe, with a queue of its own on `device` (nothing of the host's
/// submissions is touched): where a GPU upload heap's mapping lands and
/// what Windows says of it, whether guards and the write watch can work
/// on it, how fast the processor writes and reads it against the PC's
/// memory, whether the card sees the processor's bytes and the processor
/// the card's, how fast the card copies out of it against the PC's
/// memory, and (with `low`) whether such a mapping can land below 4 GiB.
/// `mb` MiB buffers. The lines are the whole account.
pub(crate) fn probe(device: &ID3D12Device, mb: usize, low: Option<LowRoom>) -> Vec<String> {
    let mut lines = Vec::new();
    if !heaps_supported(device) {
        lines.push("the device has no GPU upload heaps (OPTIONS16): nothing to probe".to_owned());
        return lines;
    }
    let bytes = mb << 20;
    let mut rig = match crate::inplace::Rig::new(device) {
        Ok(rig) => rig,
        Err(e) => {
            lines.push(format!("no queue of its own: {e}"));
            return lines;
        }
    };
    let started = Instant::now();
    let (upload_heap, at) = match make(device, bytes as u64) {
        Ok(made) => made,
        Err(e) => {
            lines.push(format!("a buffer of {mb} MiB in a GPU upload heap: REFUSED {e}"));
            return lines;
        }
    };
    lines.push(format!(
        "a buffer of {mb} MiB made and mapped in {:.1} ms, at {:#x} ({}); {}",
        started.elapsed().as_secs_f64() * 1e3,
        at as usize,
        placement(at as usize, bytes),
        query(at)
    ));
    // The processor's writes and reads, against the PC's memory.
    let source: Vec<u8> = (0..bytes).map(|i| (i as u8).wrapping_mul(13).wrapping_add(7)).collect();
    let mut ram = vec![0u8; bytes];
    let upload = rig.committed(D3D12_HEAP_TYPE_UPLOAD, bytes as u64, D3D12_RESOURCE_STATE_GENERIC_READ).and_then(|b| Ok((mapped(&b)?, b)));
    let back = write_back(device, bytes as u64);
    {
        let (vram_whole, vram_fills) = write_speeds(at, &source);
        let (ram_whole, ram_fills) = write_speeds(ram.as_mut_ptr(), &source);
        let mut line = format!(
            "the processor writes {mb} MiB: into it {:.1} GB/s whole, {:.1} GB/s in fills of 480 bytes; into the PC's memory {:.1} and {:.1}",
            gbps(bytes, vram_whole),
            gbps(bytes, vram_fills) * 480.0 / 512.0,
            gbps(bytes, ram_whole),
            gbps(bytes, ram_fills) * 480.0 / 512.0
        );
        if let Ok((data, _)) = &upload {
            let (whole, fills) = write_speeds(*data, &source);
            line += &format!("; into upload memory {:.1} and {:.1}", gbps(bytes, whole), gbps(bytes, fills) * 480.0 / 512.0);
        }
        if let Ok((_, data)) = &back {
            let (whole, fills) = write_speeds(*data, &source);
            line += &format!("; into a write-back host copy {:.1} and {:.1}", gbps(bytes, whole), gbps(bytes, fills) * 480.0 / 512.0);
        }
        lines.push(line);
    }
    // What the checks below read and write: 1 MiB, or a quarter of a
    // smaller buffer (they work at offsets of 4 and 8 KiB).
    let n = (bytes / 4).min(1 << 20);
    {
        let (plain, streaming) = read_speeds(at, n);
        let (ram_plain, _) = read_speeds(ram.as_ptr(), n);
        let ms = |s: f64| s * 1e3;
        let mut line = format!(
            "the processor reads {} KiB of it: {:.2} ms with plain loads ({:.3} GB/s), {} with streaming loads; the PC's memory {:.3} ms",
            n >> 10,
            ms(plain),
            gbps(n, plain),
            streaming.map_or_else(|| "?".to_owned(), |s| format!("{:.2} ms ({:.3} GB/s)", ms(s), gbps(n, s))),
            ms(ram_plain)
        );
        if let Ok((data, _)) = &upload {
            let (plain, streaming) = read_speeds(*data, n);
            line += &format!(
                "; upload memory {:.2} ms plain, {} streaming",
                ms(plain),
                streaming.map_or_else(|| "?".to_owned(), |s| format!("{:.2} ms", ms(s)))
            );
        }
        lines.push(line);
    }
    drop(ram);
    // The processor's write-combined stores are out before the card reads.
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    // The card sees the processor's bytes, the processor the card's.
    {
        // The pattern from the write tests is in place.
        let seen = rig.fetch(&upload_heap, 4096, n).is_ok_and(|got| got[..] == source[4096..4096 + n]);
        let stored = rig
            .committed(D3D12_HEAP_TYPE_DEFAULT, n as u64, D3D12_RESOURCE_STATE_COMMON)
            .and_then(|default| {
                if let Ok((data, up)) = &upload {
                    // SAFETY: the upload mapping holds `bytes` bytes, more
                    // than 8 KiB and `n`.
                    unsafe { std::ptr::copy_nonoverlapping(source.as_ptr().add(8192), *data, n) };
                    // SAFETY: live buffers, ranges inside them.
                    rig.run(|list| unsafe { list.CopyBufferRegion(&default, 0, up, 0, n as u64) })?;
                }
                // SAFETY: live buffers, ranges inside them.
                rig.run(|list| unsafe { list.CopyBufferRegion(&upload_heap, 0, &default, 0, n as u64) })
            })
            .is_ok();
        let mut got = vec![0u8; n];
        // SAFETY: the mapping holds `n` bytes and more.
        unsafe { std::ptr::copy_nonoverlapping(at.cast_const(), got.as_mut_ptr(), n) };
        let back_ok = stored && upload.is_ok() && got[..] == source[8192..8192 + n];
        lines.push(format!(
            "the card reads what the processor wrote through the mapping: {seen}; the processor reads what the card copied into it: {back_ok}"
        ));
    }
    // How fast the card copies out of it, against the PC's memory.
    {
        let target = rig.committed(D3D12_HEAP_TYPE_DEFAULT, bytes as u64, D3D12_RESOURCE_STATE_COMMON);
        let speed = |ms: windows::core::Result<f64>| match ms {
            Ok(ms) => format!("{:.2} ms ({:.0} GB/s)", ms, gbps(bytes, ms / 1e3)),
            Err(e) => format!("? ({e})"),
        };
        if let Ok(target) = &target {
            let own = speed(copy_gpu_ms(&mut rig, target, &upload_heap, bytes as u64));
            let up = match &upload {
                Ok((_, b)) => speed(copy_gpu_ms(&mut rig, target, b, bytes as u64)),
                Err(e) => format!("? ({e})"),
            };
            let wb = match &back {
                Ok((b, _)) => speed(copy_gpu_ms(&mut rig, target, b, bytes as u64)),
                Err(e) => format!("? ({e})"),
            };
            let card = rig
                .committed(D3D12_HEAP_TYPE_DEFAULT, bytes as u64, D3D12_RESOURCE_STATE_COMMON)
                .map_or_else(|e| format!("? ({e})"), |from| speed(copy_gpu_ms(&mut rig, target, &from, bytes as u64)));
            lines.push(format!(
                "the card copies {mb} MiB into its own memory (by its clock): from a GPU upload heap {own}; from upload memory {up}; \
                 from a write-back host copy {wb}; from its own memory {card}"
            ));
        }
    }
    drop(upload);
    drop(back);
    drop(upload_heap);
    // What the runtime's guards and the write watch would meet there, on a
    // buffer of its own that nothing writes afterwards (a page left
    // read-only would fault the next write).
    match make(device, 64 << 10) {
        Ok((small, at)) => {
            let mut old = 0;
            // SAFETY: a page of our own mapping, given back its protection.
            let guarded = unsafe { mem::VirtualProtect(at.cast(), 4096, mem::PAGE_READONLY, &mut old) } != 0;
            let error = std::io::Error::last_os_error();
            let restored = guarded && {
                let mut again = 0;
                // SAFETY: as above.
                unsafe { mem::VirtualProtect(at.cast(), 4096, old, &mut again) != 0 }
            };
            let (mut found, mut count, mut granularity) = ([0usize; 16], 16usize, 0u32);
            // SAFETY: our own mapping and out buffers of the given sizes.
            let watched =
                unsafe { mem::GetWriteWatch(0, at.cast(), 64 << 10, found.as_mut_ptr().cast(), &mut count, &mut granularity) } == 0;
            lines.push(format!(
                "a page of such a mapping made read-only (as a guard does): {}{}; given back its protection {:#x}: {}; the write watch asked \
                 of it: {}",
                guarded,
                if guarded { String::new() } else { format!(" ({error})") },
                old,
                restored,
                if watched { "answers" } else { "fails (not write-watched memory)" }
            ));
            drop(small);
        }
        Err(e) => lines.push(format!("a small buffer for the guard test: REFUSED {e}")),
    }
    // Below 4 GiB, where the title's 32-bit memory has to be (its 174 MB
    // buffer of constants and shader data is there).
    match low {
        None => lines.push("below 4 GiB: not tried (the runtime gave no way to make room there)".to_owned()),
        Some(room) => {
            const LOW: usize = 192 << 20;
            let mut error = None;
            let got = room(LOW, &mut || match make(device, LOW as u64) {
                Ok((buffer, data)) => {
                    drop(buffer);
                    data as usize
                }
                Err(e) => {
                    error = Some(e);
                    0
                }
            });
            lines.push(match (got, error) {
                (0, Some(e)) => format!("below 4 GiB: the buffer of {} MiB was REFUSED ({e})", LOW >> 20),
                (0, None) => format!("below 4 GiB: no room of {} MiB could be made", LOW >> 20),
                (at, _) => format!(
                    "below 4 GiB: with room made, a {} MiB buffer was mapped at {at:#x} ({}): {}",
                    LOW >> 20,
                    placement(at, LOW),
                    if at + LOW <= 1 << 32 {
                        "it landed BELOW 4 GiB (a 32-bit title pool could be the card's memory)"
                    } else {
                        "it did not land below 4 GiB (the kernel places such mappings elsewhere)"
                    }
                ),
            });
        }
    }
    lines
}

/// The frame line's part: what [`buffers`] put in the card's memory, and
/// (with [`tally_on`]) the tally of title reservations.
pub(crate) fn line(frames: f64) {
    if level() == 1 {
        let n = MADE.each_ref().map(|m| m.load(Relaxed));
        tracing::info!(
            "GPU upload heaps (VICTORY_GPU_UPLOAD_HEAP=1{}): {} buffers made in the card's memory since the start ({} MB: scratch memory and \
             texture staging), {} refused and made in upload memory ({} MB)",
            if buffers() { "" } else { ", NOT in use" },
            n[0],
            n[1] >> 20,
            n[2],
            n[3] >> 20
        );
    }
    if tally_on() {
        tally_line(frames);
    }
}

// The tally (`VICTORY_GPU_UPLOAD_TALLY=1`): per title reservation of 1 MiB
// or more that the runtime placed (`XMemVirtualAlloc` at an address of its
// choice), what this layer does with it. Logging only. It says which
// reservations could be the card's memory with the title writing them
// through the BAR, and what that would take away: the bytes copied into
// host copies, the layer's own reads (fills of constants, the title's
// buffer copies done on the processor, texture refreshes), and how the
// title maps pages there (views, remaps, pages shown at more than one
// address, DirectStorage writes).

/// Counters of a reservation, since the last line.
const REFRESHES: usize = 0;
const ASKED: usize = 1;
const COPIED: usize = 2;
const REFRESH_US: usize = 3;
const FILLS: usize = 4;
const FILL_BYTES: usize = 5;
const COPIES: usize = 6;
const COPY_BYTES: usize = 7;
const TEXTURES: usize = 8;
const TEXTURE_BYTES: usize = 9;
const VIEWS: usize = 10;
const VIEW_BYTES: usize = 11;
const REMAPS: usize = 12;
const ALIASED: usize = 13;
const PAGE_WRITES: usize = 14;
const PAGE_BYTES: usize = 15;
const COUNTERS: usize = 16;

struct Reservation {
    start: usize,
    len: usize,
    kind: u32,
    flags: u64,
    protect: u32,
    /// Milliseconds after the start it was made.
    at_ms: u64,
    /// Whether the host device existed then: a reservation made before it
    /// could not have been placed in the card's memory at that moment.
    after_device: bool,
    counts: [AtomicU64; COUNTERS],
    /// Since the start: views mapped, pages seen at more than one address.
    views_ever: AtomicU64,
    aliased_ever: AtomicU64,
}

/// Reservations by start, in order (a binary search finds an address's).
static TALLY: RwLock<Vec<Reservation>> = RwLock::new(Vec::new());

/// The smallest reservation tallied.
const LEAST: usize = 1 << 20;

/// The runtime reserved `[at, at + len)` for the title at an address of
/// its choice, with the title's `kind`, XMem `flags` and console
/// `protect` (from `crate::census::title_memory_reserved`).
pub(crate) fn reserved(at: usize, len: usize, kind: u32, flags: u64, protect: u32) {
    if !tally_on() || len < LEAST {
        return;
    }
    let after_device = SUPPORT.get().is_some();
    let mut tally = TALLY.write().unwrap_or_else(|e| e.into_inner());
    let i = tally.partition_point(|r| r.start < at);
    // A reservation released and made again at the same place: replaced.
    if tally.get(i).is_some_and(|r| r.start == at) {
        tally.remove(i);
    }
    tally.insert(
        i,
        Reservation {
            start: at,
            len,
            kind,
            flags,
            protect,
            at_ms: crate::host::now_ms(),
            after_device,
            counts: [const { AtomicU64::new(0) }; COUNTERS],
            views_ever: AtomicU64::new(0),
            aliased_ever: AtomicU64::new(0),
        },
    );
    if len >= 16 << 20 {
        tracing::info!(
            "GPU upload heap tally: title reservation {at:#x}+{} MB, kind {kind:#x} ({}), flags {flags:#x}{}, protect {protect:#x} ({}), \
             made {} the host device",
            len >> 20,
            kind_words(kind),
            if flags & 4 != 0 { " (32-bit)" } else { "" },
            crate::census::protect_words(protect),
            if after_device { "after" } else { "BEFORE" }
        );
    }
}

/// What the title's `kind` asked for, in words.
fn kind_words(kind: u32) -> &'static str {
    const PAGES_64K: u32 = 0x2040_0000;
    match (kind & PAGES_64K == PAGES_64K, kind & 0x1000 != 0) {
        (true, false) => "64 KiB pages reserved for physical pages",
        (true, true) => "64 KiB pages committed",
        (false, true) => "committed",
        (false, false) => "reserved",
    }
}

/// Adds `by` to counters `which` of the reservation holding `at`.
fn count(at: usize, which: &[(usize, u64)]) {
    let tally = TALLY.read().unwrap_or_else(|e| e.into_inner());
    let i = tally.partition_point(|r| r.start <= at);
    let Some(r) = i.checked_sub(1).map(|i| &tally[i]).filter(|r| at < r.start + r.len) else {
        return;
    };
    for &(counter, by) in which {
        r.counts[counter].fetch_add(by, Relaxed);
    }
}

/// A host copy brought `[from, from + len)` up to date: `copied` bytes in
/// `us` microseconds.
pub(crate) fn refreshed(from: usize, len: usize, copied: usize, us: u64) {
    if tally_on() {
        count(from, &[(REFRESHES, 1), (ASKED, len as u64), (COPIED, copied as u64), (REFRESH_US, us)]);
    }
}

/// What this layer reads of title memory on the processor.
#[derive(Clone, Copy)]
pub(crate) enum Read {
    /// A fill of scratch memory (constants of a table or a root view).
    Fill,
    /// A title buffer copy done on the processor (its source).
    Copy,
    /// A texture refresh (hashing and detiling).
    Texture,
}

/// This layer reads `[at, at + len)` of title memory on the processor.
pub(crate) fn layer_read(at: usize, len: usize, what: Read) {
    if !tally_on() {
        return;
    }
    let which = match what {
        Read::Fill => [(FILLS, 1), (FILL_BYTES, len as u64)],
        Read::Copy => [(COPIES, 1), (COPY_BYTES, len as u64)],
        Read::Texture => [(TEXTURES, 1), (TEXTURE_BYTES, len as u64)],
    };
    count(at, &which);
}

/// The title mapped physical pages at `[at, at + len)`; `aliased` of its
/// 64 KiB pages show at another address too.
pub(crate) fn view_mapped(at: usize, len: usize, aliased: usize) {
    if !tally_on() {
        return;
    }
    count(at, &[(VIEWS, 1), (VIEW_BYTES, len as u64), (ALIASED, aliased as u64)]);
    let tally = TALLY.read().unwrap_or_else(|e| e.into_inner());
    let i = tally.partition_point(|r| r.start <= at);
    if let Some(r) = i.checked_sub(1).map(|i| &tally[i]).filter(|r| at < r.start + r.len) {
        r.views_ever.fetch_add(1, Relaxed);
        r.aliased_ever.fetch_add(aliased as u64, Relaxed);
    }
}

/// What `[at, at + len)` showed is gone or replaced.
pub(crate) fn remapped(at: usize, _len: usize) {
    if tally_on() {
        count(at, &[(REMAPS, 1)]);
    }
}

/// DirectStorage (or the runtime) wrote `[at, at + len)` through a view of
/// its own.
pub(crate) fn page_written(at: usize, len: usize) {
    if tally_on() {
        count(at, &[(PAGE_WRITES, 1), (PAGE_BYTES, len as u64)]);
    }
}

/// The tally's lines: the reservations this layer works most on, per
/// frame over `frames`; the counters start again.
fn tally_line(frames: f64) {
    struct Row {
        line: String,
        weight: u64,
    }
    let tally = TALLY.read().unwrap_or_else(|e| e.into_inner());
    let mb = |b: u64| b as f64 / 1e6 / frames;
    let mut rows: Vec<Row> = tally
        .iter()
        .filter_map(|r| {
            let c: [u64; COUNTERS] = std::array::from_fn(|i| r.counts[i].swap(0, Relaxed));
            let weight = c[COPIED] + c[FILL_BYTES] + c[COPY_BYTES] + c[TEXTURE_BYTES] + c[ASKED] / 64;
            if weight == 0 && c[VIEWS] == 0 && c[REMAPS] == 0 && c[PAGE_WRITES] == 0 {
                return None;
            }
            let (textures, texture_bytes) = crate::texture::staged_between(r.start, r.start + r.len);
            Some(Row {
                weight,
                line: format!(
                    "{:#x}+{}MB [{}{}, {}, made at {:.1} s {} the device]: per frame {:.1} refreshes asking {:.1} MB, {:.2} MB copied in {:.2} ms; \
                     the layer reads {:.0} fills {:.1} KB, {:.1} buffer copies {:.1} KB, {:.1} texture refreshes {:.2} MB; textures staged there now {} \
                     ({:.0} MB); since the line before: {} views mapped ({:.0} MB), {} remaps, {} pages shown at another address too, {} DirectStorage \
                     writes ({:.1} MB); since the start {} views, {} pages shown elsewhere too",
                    r.start,
                    r.len >> 20,
                    kind_words(r.kind),
                    if r.flags & 4 != 0 { ", 32-bit" } else { "" },
                    crate::census::protect_words(r.protect),
                    r.at_ms as f64 / 1e3,
                    if r.after_device { "after" } else { "BEFORE" },
                    c[REFRESHES] as f64 / frames,
                    mb(c[ASKED]),
                    mb(c[COPIED]),
                    c[REFRESH_US] as f64 / 1e3 / frames,
                    c[FILLS] as f64 / frames,
                    c[FILL_BYTES] as f64 / 1e3 / frames,
                    c[COPIES] as f64 / frames,
                    c[COPY_BYTES] as f64 / 1e3 / frames,
                    c[TEXTURES] as f64 / frames,
                    mb(c[TEXTURE_BYTES]),
                    textures,
                    texture_bytes as f64 / 1e6,
                    c[VIEWS],
                    c[VIEW_BYTES] as f64 / 1e6,
                    c[REMAPS],
                    c[ALIASED],
                    c[PAGE_WRITES],
                    c[PAGE_BYTES] as f64 / 1e6,
                    r.views_ever.load(Relaxed),
                    r.aliased_ever.load(Relaxed),
                ),
            })
        })
        .collect();
    let count = tally.len();
    drop(tally);
    rows.sort_by_key(|r| std::cmp::Reverse(r.weight));
    tracing::info!(
        "GPU upload heap tally (VICTORY_GPU_UPLOAD_TALLY=1): {count} title reservations of 1 MB or more; the busiest of the {} with work since the line before:",
        rows.len()
    );
    for row in rows.iter().take(12) {
        tracing::info!("GPU upload heap tally:   {}", row.line);
    }
}

/// Pages of `views` (start → list of addresses showing each page) shown at
/// more than one address: for [`view_mapped`]'s count, from the runtime's
/// views as `crate::inplace` keeps them.
pub(crate) fn aliased_pages<'a>(shown: impl Iterator<Item = &'a Vec<usize>>) -> usize {
    shown.filter(|addresses| addresses.len() > 1).count()
}

/// The reservations tallied now, as (start, bytes) (tests).
#[cfg(test)]
fn tallied() -> Vec<(usize, usize)> {
    TALLY.read().unwrap_or_else(|e| e.into_inner()).iter().map(|r| (r.start, r.len)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_switch_reads_one_probe_and_off() {
        assert_eq!(parse_level(None), 0);
        assert_eq!(parse_level(Some("0")), 0);
        assert_eq!(parse_level(Some("1")), 1);
        assert_eq!(parse_level(Some(" probe ")), 2);
        assert_eq!(parse_level(Some("yes")), 0);
    }

    /// Counts land on the reservation that holds the address, none
    /// elsewhere; one made again at the same place replaces the old.
    #[test]
    fn counts_land_on_the_reservation_that_holds_them() {
        let _one = TEST.lock().unwrap_or_else(|e| e.into_inner());
        let base = 0x7e_0000_0000usize;
        let insert = |start: usize, len: usize| {
            let mut tally = TALLY.write().unwrap();
            let i = tally.partition_point(|r| r.start < start);
            if tally.get(i).is_some_and(|r| r.start == start) {
                tally.remove(i);
            }
            tally.insert(
                i,
                Reservation {
                    start,
                    len,
                    kind: 0x3000,
                    flags: 0,
                    protect: 0x404,
                    at_ms: 0,
                    after_device: true,
                    counts: [const { AtomicU64::new(0) }; COUNTERS],
                    views_ever: AtomicU64::new(0),
                    aliased_ever: AtomicU64::new(0),
                },
            );
        };
        insert(base + (8 << 20), 4 << 20);
        insert(base, 2 << 20);
        insert(base + (4 << 20), 2 << 20);
        assert_eq!(&tallied()[..3], &[(base, 2 << 20), (base + (4 << 20), 2 << 20), (base + (8 << 20), 4 << 20)]);
        count(base + 100, &[(FILLS, 1), (FILL_BYTES, 480)]);
        count(base + (2 << 20), &[(FILLS, 1)]); // in the gap: nowhere
        count(base + (9 << 20), &[(COPIED, 4096)]);
        count(base - 1, &[(FILLS, 1)]); // below all: nowhere
        {
            let tally = TALLY.read().unwrap();
            let at = |start: usize| tally.iter().find(|r| r.start == start).unwrap();
            assert_eq!(at(base).counts[FILLS].load(Relaxed), 1);
            assert_eq!(at(base).counts[FILL_BYTES].load(Relaxed), 480);
            assert_eq!(at(base + (4 << 20)).counts[FILLS].load(Relaxed), 0);
            assert_eq!(at(base + (8 << 20)).counts[COPIED].load(Relaxed), 4096);
        }
        insert(base, 1 << 20);
        assert_eq!(tallied().iter().filter(|r| r.0 == base).count(), 1);
        assert_eq!(tallied().iter().find(|r| r.0 == base).unwrap().1, 1 << 20);
        TALLY.write().unwrap().retain(|r| r.start < base || r.start >= base + (16 << 20));
    }

    #[test]
    fn pages_shown_at_two_addresses_are_aliased() {
        let shown = [vec![1usize], vec![2, 3], vec![4], vec![5, 6, 7]];
        assert_eq!(aliased_pages(shown.iter()), 2);
    }

    /// The probe on the software adapter, whole: support is found, and
    /// where the adapter has GPU upload heaps a buffer in one is made, the
    /// card reads what the processor wrote there and the processor what
    /// the card copied in. The adapter's speeds say nothing of a card's.
    #[test]
    fn warp_probe_runs_where_the_adapter_has_upload_heaps() {
        let Some(device) = crate::inplace::tests::warp() else {
            eprintln!("no software adapter here");
            return;
        };
        let supported = heaps_supported(&device);
        eprintln!("the software adapter has GPU upload heaps: {supported}");
        let lines = probe(&device, 4, None);
        for line in &lines {
            eprintln!("probe: {line}");
        }
        if !supported {
            assert!(lines[0].contains("no GPU upload heaps"));
            return;
        }
        assert!(lines.iter().any(|l| l.contains("made and mapped")), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("the card reads what the processor wrote through the mapping: true; the processor reads what the card copied into it: true")),
            "{lines:?}"
        );
        assert!(kernel_copy_works(&device), "the kernel's copy writes into a GPU upload heap");
    }

    /// With the switch, on the host device of the software adapter
    /// (diagnostics builds with `VICTORY_D3D12_WARP=1` only: elsewhere the
    /// host device is the card, which tests leave alone): a buffer the
    /// processor only writes is in a GPU upload heap, in the common state,
    /// and the card reads what was written into it. Run after the other
    /// tests of the crate (their names come first), it says how many such
    /// buffers they made.
    #[test]
    fn warp_host_buffers_written_by_the_processor_go_to_upload_heaps() {
        if !cfg!(feature = "diagnostics") || std::env::var_os("VICTORY_D3D12_WARP").is_none() || level() != 1 {
            eprintln!("needs a diagnostics build, VICTORY_D3D12_WARP=1 and VICTORY_GPU_UPLOAD_HEAP=1");
            return;
        }
        let Some(host) = crate::host::host() else {
            return;
        };
        eprintln!(
            "buffers in GPU upload heaps made so far by this process's tests: {} ({} KiB), refused {}",
            MADE[0].load(Relaxed),
            MADE[1].load(Relaxed) >> 10,
            MADE[2].load(Relaxed)
        );
        assert!(buffers(), "the software adapter has GPU upload heaps and the kernel's copy writes into them");
        let (buffer, data) = host.written_buffer(64 << 10).expect("a buffer");
        let mut properties = D3D12_HEAP_PROPERTIES::default();
        // SAFETY: a plain query of a live committed buffer.
        unsafe { buffer.GetHeapProperties(Some(&raw mut properties), None) }.expect("heap properties");
        assert_eq!(properties.Type, D3D12_HEAP_TYPE_GPU_UPLOAD);
        let bytes: Vec<u8> = (0..4096u32).map(|i| (i * 7) as u8).collect();
        // SAFETY: the mapping holds 64 KiB.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.add(512), bytes.len()) };
        let mut rig = crate::inplace::Rig::new(&host.device).expect("a rig");
        assert_eq!(rig.fetch(&buffer, 512, bytes.len()).expect("fetched"), bytes);
    }

    /// With room made below 4 GiB by a stand-in for the runtime's guards,
    /// the probe says where the mapping landed.
    #[test]
    fn warp_probe_says_where_a_low_mapping_landed() {
        let Some(device) = crate::inplace::tests::warp() else {
            return;
        };
        if !heaps_supported(&device) {
            return;
        }
        fn room(bytes: usize, place: &mut dyn FnMut() -> usize) -> usize {
            assert_eq!(bytes, 192 << 20);
            place()
        }
        let lines = probe(&device, 1, Some(room));
        let low = lines.iter().find(|l| l.starts_with("below 4 GiB")).expect("a line for below 4 GiB");
        eprintln!("{low}");
        assert!(low.contains("was mapped at") || low.contains("REFUSED"), "{low}");
    }
}
