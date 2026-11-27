//! The buffer views a translated shader moves 16-bit channels through as unsigned integers
//! (`victory_shader::emit::HostShader::half_views`; fxceless #927, tippythetaidum).
//!
//! On the console a 16-bit typed buffer access moves each channel's bits as they are, and the
//! title keeps half floats (and 16-bit integers beside them) in `R16G16B16A16_UINT` and
//! `R16_UINT` buffers. The translator moves such channels as unsigned integers
//! (`bufferLoad.i32`, `bufferStore.i32`) through the descriptors it names; an unsigned 16-bit
//! view carries every pattern unchanged, and a 16-bit float or signed one is shown to that
//! shader as the unsigned format of the same layout ([`half_view`], made in the fresh table
//! `draw::State::tables` makes for the draw: the title's own slot keeps the title's format for
//! the shaders that read it as floats). The title's own views there are unsigned already, so
//! for the title no table is made afresh for this.

use super::View;

/// Buffer views: dimension 1 in the title's description of either kind.
const BUFFER: u32 = 1;

/// The unsigned integer format with the layout of a 16-bit float or signed integer format (one,
/// two or four channels), if `format` is one.
pub(crate) fn half_format(format: u32) -> Option<u32> {
    match format {
        // R16G16B16A16_FLOAT and _SINT: R16G16B16A16_UINT.
        10 | 14 => Some(12),
        // R16G16_FLOAT and _SINT: R16G16_UINT.
        34 | 38 => Some(36),
        // R16_FLOAT and _SINT: R16_UINT.
        54 | 59 => Some(57),
        _ => None,
    }
}

/// `view` as a shader is shown it where it moves a buffer's 16-bit channels as unsigned
/// integers: a buffer view in a 16-bit float or signed format, in the unsigned format of the
/// same layout, through which every 16-bit pattern is read and stored as it is. `None` for
/// every other view: the title's own serves (an unsigned one already carries the bits; a
/// texture can only be viewed in the formats it was made for).
pub(crate) fn half_view(view: &View) -> Option<View> {
    match view {
        View::Shader(resource, Some(desc)) if desc[1] == BUFFER => {
            let mut desc = *desc;
            desc[0] = half_format(desc[0])?;
            Some(View::Shader(*resource, Some(desc)))
        }
        View::Unordered(resource, counter, Some(desc)) if desc[1] == BUFFER => {
            let mut desc = *desc;
            desc[0] = half_format(desc[0])?;
            Some(View::Unordered(*resource, *counter, Some(desc)))
        }
        View::Placed(unordered, resource, desc) if resource[0] == BUFFER && desc[1] == BUFFER => {
            let mut desc = *desc;
            desc[0] = half_format(desc[0])?;
            Some(View::Placed(*unordered, *resource, desc))
        }
        _ => None,
    }
}

/// The format of `view`, a buffer view through which a shader moves 16-bit channels as
/// unsigned integers, if those integers are not what the console gives: a format of wider
/// channels, a normalised one or a signed 8-bit one, whose values the console converts to and
/// from halves. `None` for the formats the integers serve exactly (16 bits a channel of any
/// kind, through [`half_view`] where needed, and unsigned channels of fewer bits; the title's
/// are 16-bit unsigned) and for any other view.
pub(crate) fn half_inexact(view: &View) -> Option<u32> {
    // R16G16B16A16, R16G16 and R16 (float, unsigned, signed), then R10G10B10A2, R8G8B8A8, R8G8
    // and R8 unsigned.
    const EXACT: [u32; 13] = [10, 12, 14, 34, 36, 38, 54, 57, 59, 25, 30, 50, 62];
    let format = match view {
        View::Shader(_, Some(desc)) | View::Unordered(_, _, Some(desc)) if desc[1] == BUFFER => {
            desc[0]
        }
        View::Placed(_, resource, desc) if resource[0] == BUFFER && desc[1] == BUFFER => desc[0],
        _ => return None,
    };
    (!EXACT.contains(&format)).then_some(format)
}

/// Whether the table at `table` holds, at one of the dwords `half` (the places a shader moves
/// halves through), a view the shader is to be shown as its unsigned twin ([`half_view`]): such
/// a table is made afresh for the draw. A view there whose values the integers do not carry
/// ([`half_inexact`]) is logged once with its format.
#[cfg(windows)]
pub(crate) fn twinned_table(table: u64, half: &[u32]) -> bool {
    static INEXACT: std::sync::Once = std::sync::Once::new();
    let mut twinned = false;
    for &at in half {
        let Some(view) = super::view(table + u64::from(at) * 4) else {
            continue;
        };
        if half_view(&view).is_some() {
            twinned = true;
        } else if let Some(format) = half_inexact(&view) {
            INEXACT.call_once(|| {
                tracing::warn!(
                    format,
                    "a shader moves a buffer's 16-bit channels as integers through a view whose values the console converts: they are read and written unconverted"
                );
            });
        }
    }
    twinned
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unsigned twin of a 16-bit float or signed format has its layout, and only buffer
    /// views in such a format are shown as another view.
    #[test]
    fn only_16_bit_float_and_signed_buffer_views_get_an_unsigned_twin() {
        for (format, unsigned) in [(10, 12), (14, 12), (34, 36), (38, 36), (54, 57), (59, 57)] {
            assert_eq!(half_format(format), Some(unsigned));
            assert_eq!(
                crate::resource::element(format),
                crate::resource::element(unsigned),
                "format {format}"
            );
        }
        // The unsigned formats themselves, normalised 16-bit formats, and wider and narrower
        // ones.
        for other in [0, 12, 36, 57, 11, 13, 35, 37, 56, 58, 42, 17, 30, 61] {
            assert_eq!(half_format(other), None, "format {other}");
        }
        let words = |format: u32, dimension: u32| [format, dimension, 5768, 0, 0, 0, 64, 0, 0, 0];
        // R16G16B16A16_FLOAT: a buffer of halves a program moves as they are.
        assert_eq!(
            half_view(&View::Unordered(7, 0, Some(words(10, BUFFER)))),
            Some(View::Unordered(7, 0, Some(words(12, BUFFER))))
        );
        assert_eq!(
            half_view(&View::Shader(7, Some(words(54, BUFFER)))),
            Some(View::Shader(7, Some(words(57, BUFFER))))
        );
        let mut placed = [0u32; 16];
        placed[..10].copy_from_slice(&words(38, BUFFER));
        placed[10] = 0x1234_0000;
        let mut twin = placed;
        twin[0] = 36;
        let memory = [BUFFER, 0, 0, 0, 512, 0, 1, 0x1_0001, 0, 1, 0, 1, 0, 0];
        assert_eq!(
            half_view(&View::Placed(true, memory, placed)),
            Some(View::Placed(true, memory, twin))
        );
        // The title's kind (R16G16B16A16_UINT and R16_UINT buffers), a texture view
        // (dimension 4: 2D), a view without a description, a constant buffer: the title's own.
        assert_eq!(
            half_view(&View::Unordered(7, 0, Some(words(12, BUFFER)))),
            None
        );
        assert_eq!(half_view(&View::Shader(7, Some(words(57, BUFFER)))), None);
        assert_eq!(half_view(&View::Unordered(7, 0, Some(words(10, 4)))), None);
        assert_eq!(half_view(&View::Shader(7, None)), None);
        assert_eq!(half_view(&View::Constant(0x1000, 256)), None);
    }

    /// The buffer views through which the integers a shader moves halves as are not what the
    /// console gives are named; the title's are not.
    #[test]
    fn views_whose_values_the_console_would_convert_are_named() {
        let words = |format: u32, dimension: u32| [format, dimension, 5768, 0, 0, 0, 64, 0, 0, 0];
        for exact in [10, 12, 14, 34, 36, 38, 54, 57, 59, 25, 30, 50, 62] {
            let view = View::Unordered(7, 0, Some(words(exact, BUFFER)));
            assert_eq!(half_inexact(&view), None, "format {exact}");
        }
        // R32_FLOAT, R32_UINT, R16G16B16A16_UNORM, R8G8B8A8_UNORM, R8_SINT, and a structured or
        // raw view (no format).
        for inexact in [41, 42, 11, 28, 64, 0] {
            let view = View::Shader(7, Some(words(inexact, BUFFER)));
            assert_eq!(half_inexact(&view), Some(inexact));
        }
        let mut placed = [0u32; 16];
        placed[..10].copy_from_slice(&words(41, BUFFER));
        let memory = [BUFFER, 0, 0, 0, 512, 0, 1, 0x1_0001, 0, 1, 0, 1, 0, 0];
        assert_eq!(half_inexact(&View::Placed(true, memory, placed)), Some(41));
        // Not a buffer view.
        assert_eq!(half_inexact(&View::Shader(7, Some(words(41, 4)))), None);
        assert_eq!(half_inexact(&View::Constant(0x1000, 256)), None);
    }

    /// A table made afresh for a draw holds the descriptors its shader moves halves through
    /// (`HostShader::half_views`), also past the range starts its used-resource entries give:
    /// with one entry for a range from dword 0 and a 16-bit float buffer view at dword 8, the
    /// table takes two dynamic slots, so the next allocation does not land on the view's. (The
    /// case avt0m4t48 found in review on #927.) The view made there is the unsigned twin.
    #[cfg(windows)]
    #[test]
    fn a_fresh_table_holds_the_half_views_past_its_range_starts() {
        use super::super::{
            HeapDesc, Placement, SHADER_VISIBLE, VIEWS_KIND, create_heap, create_placed_srv, heap,
        };
        use crate::object::{S_OK, This, slot};
        use victory_shader::{ResourceType, RootType, UsedResource};
        let Some(device) = crate::host::host() else {
            return; // no host GPU
        };
        let list = device.new_list().expect("a host list");
        #[repr(C)]
        struct Extended(HeapDesc, Placement);
        let extended = Extended(
            HeapDesc {
                kind: VIEWS_KIND,
                count: 2,
                flags: SHADER_VISIBLE,
                nodes: 1,
            },
            Placement {
                address: 0,
                size: 0,
            },
        );
        let mut h: This = std::ptr::null();
        assert_eq!(
            create_heap(
                std::ptr::null(),
                &extended.0,
                &super::super::HEAP_IIDS[0],
                &mut h
            ),
            S_OK
        );
        let title = heap(h);
        let host_heap = title.host().expect("a host view heap");
        let (first, second) = (title.handle(0).unwrap(), title.handle(1).unwrap());
        // An R16G16B16A16_FLOAT buffer view of memory in the second slot.
        let memory = [0u64; 64];
        let address = memory.as_ptr() as u64;
        let resource = [1, 0, 0, 0, 512, 0, 1, 0x1_0001, 0, 1, 0, 1, 0, 0];
        let mut words = [0u32; 16];
        words[..10].copy_from_slice(&[10, 1, 5768, 0, 0, 0, 64, 0, 0, 0]);
        words[10] = address as u32;
        words[11] = (address >> 32) as u32;
        create_placed_srv(std::ptr::null(), &resource, &words, second);
        assert!(
            super::super::view(second)
                .as_ref()
                .and_then(half_view)
                .is_some()
        );
        let mut state = crate::draw::State::default();
        state.set_heaps(&[h]);
        state.set_table(true, 1, first);
        let range = UsedResource {
            kind: ResourceType::Srv,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 8,
            dwords: 4,
            user_data: 4,
            slot: 1,
        };
        let plan = crate::draw::TablePlan::new(&[range], &[], &[(1, 8)]);
        let made = state.tables(&list, &plan, true);
        let [(1, offset)] = made[..] else {
            panic!("one fresh table for root parameter 1, not {:?}", &made[..]);
        };
        assert_ne!(offset, u32::MAX, "the table found no dynamic slots");
        let next = host_heap.dynamic(1).expect("a dynamic slot");
        host_heap.release(host_heap.id(), next, 1);
        drop(state);
        slot::<extern "system" fn(This) -> u32>(h, 0x10)(h);
        let (table_first, table_end) = (offset / 32, offset / 32 + 2);
        assert!(
            !(table_first..table_end).contains(&next),
            "the next allocation ({next}) took a slot of the table ({table_first}..{table_end})"
        );
    }

    /// The two ways a translated `buffer_load_format_d16_xyzw` and `buffer_store_format_d16_xyzw`
    /// have moved a typed buffer's 16-bit channels, on the host device, over a buffer that holds
    /// every 16-bit pattern. As unsigned integers through an unsigned view (the title's own
    /// R16G16B16A16_UINT, or the twin `half_view` gives a float one) every pattern comes back as
    /// it was. By way of floats, as before (load as floats, each to its half's bits; each half
    /// widened and stored as a float): through the title's integer view that is outside what
    /// D3D12 defines, and what the device gives is printed, not checked (1 pattern of 65,536 on
    /// the cards #927 tried: the network that moves the title's faces computed zeros).
    #[cfg(windows)]
    #[test]
    fn halves_cross_an_unsigned_buffer_view_unchanged() {
        use windows::Win32::Graphics::Direct3D12::*;
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
        const MOVES: &str = r#"
#define RS "DescriptorTable(UAV(u0, numDescriptors=2))"
#ifdef BY_FLOATS
RWBuffer<float4> from : register(u0);
RWBuffer<float4> to : register(u1);
#else
RWBuffer<uint4> from : register(u0);
RWBuffer<uint4> to : register(u1);
#endif
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void BY_FLOATS_OR_NOT(uint3 id : SV_DispatchThreadID) {
#ifdef BY_FLOATS
    uint4 h = f32tof16(from[id.x]);
#else
    uint4 h = from[id.x] & 0xffff;
#endif
    uint low = h.x | h.y << 16;
    uint high = h.z | h.w << 16;
#ifdef BY_FLOATS
    to[id.x] = float4(f16tof32(low), f16tof32(low >> 16), f16tof32(high), f16tof32(high >> 16));
#else
    to[id.x] = uint4(low & 0xffff, low >> 16, high & 0xffff, high >> 16);
#endif
}
"#;
        const ELEMENTS: usize = 1 << 14;
        const BYTES: u64 = ELEMENTS as u64 * 8;
        const TITLES: u32 = 12; // R16G16B16A16_UINT
        let Some(host) = crate::host::host() else {
            return; // no host GPU
        };
        let heap: ID3D12DescriptorHeap = {
            let desc = D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: 2,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            };
            // SAFETY: plain heap creation.
            unsafe { host.device.CreateDescriptorHeap(&desc) }.expect("a view heap")
        };
        // How many of the 65,536 patterns come back as they were, moved by the program
        // `by_floats` or not, through views in `format`.
        let moved = |by_floats: bool, format: u32| -> usize {
            let source = MOVES.replace("BY_FLOATS_OR_NOT", "main").replace(
                "#ifdef BY_FLOATS",
                if by_floats { "#if 1" } else { "#if 0" },
            );
            let code = crate::host::compile(
                &source,
                windows::core::s!("main"),
                windows::core::s!("cs_5_1"),
            )
            .expect("the test's shader");
            let (from, filled) = host.shared_buffer(BYTES).expect("a buffer");
            let (to, result) = host.shared_buffer(BYTES).expect("a buffer");
            // SAFETY: the shader's own root signature and plain creation calls; both mappings
            // hold `BYTES` bytes; the views are of live buffers, in slots of the heap made
            // above; an open list.
            unsafe {
                let root: ID3D12RootSignature = host
                    .device
                    .CreateRootSignature(0, &code)
                    .expect("a root signature");
                let pso: ID3D12PipelineState = host
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
                let filled = std::slice::from_raw_parts_mut(filled.cast::<u16>(), ELEMENTS * 4);
                for (pattern, half) in filled.iter_mut().enumerate() {
                    *half = pattern as u16;
                }
                std::ptr::write_bytes(result, 0xee, BYTES as usize);
                let view = D3D12_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT(format as i32),
                    ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
                    Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_UAV {
                            NumElements: ELEMENTS as u32,
                            ..Default::default()
                        },
                    },
                };
                let start = heap.GetCPUDescriptorHandleForHeapStart();
                let step = host
                    .device
                    .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)
                    as usize;
                host.device.CreateUnorderedAccessView(
                    &from,
                    None::<&ID3D12Resource>,
                    Some(&view),
                    start,
                );
                let second = D3D12_CPU_DESCRIPTOR_HANDLE {
                    ptr: start.ptr + step,
                };
                host.device.CreateUnorderedAccessView(
                    &to,
                    None::<&ID3D12Resource>,
                    Some(&view),
                    second,
                );
                let list = host.new_list().expect("a host list");
                list.list.SetDescriptorHeaps(&[Some(heap.clone())]);
                list.list.SetComputeRootSignature(&root);
                list.list.SetPipelineState(&pso);
                list.list
                    .SetComputeRootDescriptorTable(0, heap.GetGPUDescriptorHandleForHeapStart());
                list.list.Dispatch(ELEMENTS as u32 / 64, 1, 1);
                list.close();
                host.execute(&[&list]);
                assert!(
                    host.device.GetDeviceRemovedReason().is_ok(),
                    "the device, after views in format {format}"
                );
                let result = std::slice::from_raw_parts(result.cast::<u16>(), ELEMENTS * 4);
                (0..=u16::MAX)
                    .filter(|&p| result[usize::from(p)] == p)
                    .count()
            }
        };
        // A float view of halves is shown as the title's kind of view.
        assert_eq!(half_format(10), Some(TITLES));
        let unsigned = moved(false, TITLES);
        let by_floats = moved(true, TITLES);
        eprintln!(
            "halves through an R16G16B16A16_UINT buffer view: {unsigned} of 65536 patterns came back unchanged moved as unsigned integers, {by_floats} moved by way of floats"
        );
        assert_eq!(
            unsigned, 65_536,
            "patterns unchanged, moved as unsigned integers"
        );
    }
}
