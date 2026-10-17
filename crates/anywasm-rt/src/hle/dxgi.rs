//! DXGI: factory, adapter, output, swap chain.

use super::com::*;
use super::gpu::{op, Pk};
use super::*;
use crate::process::*;

const DXGI_ERROR_MORE_DATA: u64 = 0x887a_0003;
const DXGI_MODE_FORMATS: [u32; 2] = [28, 87]; // RGBA8_UNORM and BGRA8_UNORM.
const DXGI_OUTPUT_MODES: [(u32, u32); 4] = [(1280, 720), (1600, 900), (1920, 1080), (2560, 1440)];

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "CreateDXGIFactory" | "CreateDXGIFactory1" => |p| {
            let (riid, out) = (p.arg(0), p.arg(1));
            let _ = riid;
            ensure_singletons(p);
            let f = p.extra.com.factory;
            p.extra.com.objs.get_mut(&f).map(|o| o.refs += 1);
            p.wr64(out, f)?;
            Ok(S_OK)
        },
        "CreateDXGIFactory2" => |p| {
            let out = p.arg(2);
            ensure_singletons(p);
            let f = p.extra.com.factory;
            p.extra.com.objs.get_mut(&f).map(|o| o.refs += 1);
            p.wr64(out, f)?;
            Ok(S_OK)
        },
        _ => return None,
    })
}

/// Create the device-independent singletons (factory, adapter, output) once.
pub fn ensure_singletons(p: &mut Process) {
    if p.extra.com.factory == 0 {
        p.extra.com.factory = create(p, "IDXGIFactory2", Kind::Factory);
        p.extra.com.adapter = create(p, "IDXGIAdapter1", Kind::Adapter);
        p.extra.com.output = create(p, "IDXGIOutput", Kind::Output);
    }
}

pub fn method(_iface: &str, m: &str) -> Option<HleFn> {
    Some(match m {
        "EnumAdapters" | "EnumAdapters1" => |p| {
            let (idx, out) = (p.arg(1), p.arg(2));
            ensure_singletons(p);
            if idx != 0 {
                p.wr64(out, 0)?;
                return Ok(DXGI_ERROR_NOT_FOUND);
            }
            let a = p.extra.com.adapter;
            p.extra.com.objs.get_mut(&a).map(|o| o.refs += 1);
            p.wr64(out, a)?;
            Ok(S_OK)
        },
        "EnumOutputs" => |p| {
            let (idx, out) = (p.arg(1), p.arg(2));
            if idx != 0 {
                p.wr64(out, 0)?;
                return Ok(DXGI_ERROR_NOT_FOUND);
            }
            let o = p.extra.com.output;
            p.extra.com.objs.get_mut(&o).map(|x| x.refs += 1);
            p.wr64(out, o)?;
            Ok(S_OK)
        },
        "GetWindowAssociation" => |p| {
            let o = p.arg(1);
            p.wr64(o, 0)?;
            Ok(S_OK)
        },
        "GetFullscreenState" => |p| {
            let (b, o) = (p.arg(1), p.arg(2));
            if b != 0 {
                p.wr32(b, 0)?;
            }
            if o != 0 {
                p.wr64(o, 0)?;
            }
            Ok(S_OK)
        },
        // The virtual adapter list is immutable for this process lifetime.
        "IsCurrent" => |_| Ok(1),
        // The WebGPU-backed virtual output does not expose stereo presentation.
        "IsWindowedStereoEnabled" => |_| Ok(0),
        "GetMaximumFrameLatency" => |p| {
            let o = p.arg(1);
            p.wr32(o, 3)?;
            Ok(S_OK)
        },
        "GetGPUThreadPriority" => |p| {
            let o = p.arg(1);
            p.wr32(o, 0)?;
            Ok(S_OK)
        },
        "GetFrameLatencyWaitableObject" => get_frame_latency_waitable_object,
        "GetAdapter" => |p| {
            let out = p.arg(1);
            ensure_singletons(p);
            let a = p.extra.com.adapter;
            p.extra.com.objs.get_mut(&a).map(|o| o.refs += 1);
            p.wr64(out, a)?;
            Ok(S_OK)
        },
        "GetDesc1" => get_adapter_desc1,
        "CheckInterfaceSupport" => |p| {
            let o = p.arg(2);
            if o != 0 {
                p.wr64(o, 0x0000_0001_0000_0000)?;
            }
            Ok(S_OK)
        },
        "CreateSwapChain" => create_swap_chain,
        "CreateSwapChainForHwnd" => create_swap_chain_for_hwnd,
        "Present" => present,
        "Present1" => present,
        "GetBuffer" => get_buffer,
        "ResizeBuffers" => resize_buffers,
        "GetHwnd" => |p| {
            let (_t, o) = (p.arg(0), p.arg(1));
            p.wr64(o, p.extra.win.main_window)?;
            Ok(S_OK)
        },
        "GetContainingOutput" => |p| {
            let out = p.arg(1);
            ensure_singletons(p);
            let o = p.extra.com.output;
            p.extra.com.objs.get_mut(&o).map(|x| x.refs += 1);
            p.wr64(out, o)?;
            Ok(S_OK)
        },
        "GetFrameStatistics" | "GetLastPresentCount" | "GetPresentStatistics" => {
            |_| Ok(DXGI_ERROR_INVALID_CALL)
        }
        "GetDisplayModeList" => get_display_mode_list,
        "FindClosestMatchingMode" => find_closest_matching_mode,
        _ => return None,
    })
}

fn get_frame_latency_waitable_object(p: &mut Process) -> R {
    let this = p.arg(0);
    let Some(Kind::SwapChain { id, .. }) = kind_of(p, this) else {
        return Ok(0);
    };
    let handle = if let Some(&handle) = p.extra.com.frame_latency_handles.get(&id) {
        handle
    } else {
        // The virtual renderer has an immediately available frame slot. Keep a signaled manual
        // reset event for this swap chain so guest frame loops can use the standard wait path.
        let handle = p.new_handle(Handle::Event {
            signaled: true,
            manual: true,
        });
        p.extra.com.frame_latency_handles.insert(id, handle);
        handle
    };
    Ok(handle)
}

fn get_adapter_desc1(p: &mut Process) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    let mut b = vec![0u8; 312];
    for (i, c) in "AnyWASM WebGPU adapter".encode_utf16().enumerate() {
        b[2 * i..2 * i + 2].copy_from_slice(&c.to_le_bytes());
    }
    b[256..260].copy_from_slice(&0x1414u32.to_le_bytes()); // Microsoft software adapter
    b[260..264].copy_from_slice(&0x8c00u32.to_le_bytes());
    let memory = 4u64 << 30;
    b[272..280].copy_from_slice(&memory.to_le_bytes());
    b[288..296].copy_from_slice(&memory.to_le_bytes());
    // Adapter LUID and flags remain zero for the single virtual adapter.
    p.write_bytes(out, &b)?;
    Ok(S_OK)
}

fn get_display_mode_list(p: &mut Process) -> R {
    // (this, EnumFormat, Flags, UINT* pNumModes, DXGI_MODE_DESC* pDesc)
    let (fmt, count_p, desc_p) = (p.arg(1) as u32, p.arg(3), p.arg(4));
    let flags = p.arg(2) as u32;
    if count_p == 0 || flags & !0x3 != 0 {
        return Ok(DXGI_ERROR_INVALID_CALL);
    }
    if !DXGI_MODE_FORMATS.contains(&fmt) {
        p.wr32(count_p, 0)?;
        return Ok(DXGI_ERROR_NOT_FOUND);
    }

    if desc_p == 0 {
        p.wr32(count_p, DXGI_OUTPUT_MODES.len() as u32)?;
        return Ok(S_OK);
    }

    let capacity = p.rd32(count_p)? as usize;
    let written = capacity.min(DXGI_OUTPUT_MODES.len());
    for (i, (w, h)) in DXGI_OUTPUT_MODES.iter().take(written).enumerate() {
        let a = desc_p + 28 * i as u64;
        for (k, v) in [*w, *h, 60, 1, fmt, 0, 0].iter().enumerate() {
            p.wr32(a + 4 * k as u64, *v)?;
        }
    }
    p.wr32(count_p, written as u32)?;
    Ok(if written < DXGI_OUTPUT_MODES.len() {
        DXGI_ERROR_MORE_DATA
    } else {
        S_OK
    })
}

fn find_closest_matching_mode(p: &mut Process) -> R {
    // (this, const DXGI_MODE_DESC* pModeToMatch, DXGI_MODE_DESC* pClosestMatch,
    //  IUnknown* pConcernedDevice)
    let (want, out, concerned_device) = (p.arg(1), p.arg(2), p.arg(3));
    if want == 0 || out == 0 {
        return Ok(DXGI_ERROR_INVALID_CALL);
    }
    let mut requested = [0u32; 7];
    for (i, value) in requested.iter_mut().enumerate() {
        *value = p.rd32(want + i as u64 * 4)?;
    }
    let [width, height, refresh_num, refresh_den, requested_format, scanline, scaling] = requested;
    if (width == 0) != (height == 0)
        || (refresh_num == 0) != (refresh_den == 0)
        || scanline > 3
        || scaling > 2
    {
        return Ok(DXGI_ERROR_INVALID_CALL);
    }
    let format = if requested_format == 0 {
        if concerned_device == 0 {
            return Ok(DXGI_ERROR_INVALID_CALL);
        }
        if !matches!(kind_of(p, concerned_device), Some(Kind::Device)) {
            return Ok(DXGI_ERROR_INVALID_CALL);
        }
        DXGI_MODE_FORMATS[0]
    } else if DXGI_MODE_FORMATS.contains(&requested_format) {
        requested_format
    } else {
        return Ok(DXGI_ERROR_NOT_FOUND);
    };

    let target_width = if width == 0 { 1920 } else { width };
    let target_height = if height == 0 { 1080 } else { height };
    let &(best_width, best_height) = DXGI_OUTPUT_MODES
        .iter()
        .min_by_key(|&&(mode_width, mode_height)| {
            let dw = u64::from(mode_width.abs_diff(target_width));
            let dh = u64::from(mode_height.abs_diff(target_height));
            dw * dw + dh * dh
        })
        .expect("the virtual output advertises at least one mode");
    let mut closest = [0u8; 28];
    for (i, value) in [best_width, best_height, 60, 1, format, 0, 0]
        .iter()
        .enumerate()
    {
        closest[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    p.write_bytes(out, &closest)?;
    Ok(S_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vfs;
    use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};

    fn set_mode_list_args(p: &mut Process, format: u32, flags: u32, count: u64, desc: u64) {
        if p.cpu.gpr[RSP] == 0 {
            let stack = p.alloc_virtual(0x1000, anywasm_cpu::mem::PROT_RW);
            p.cpu.gpr[RSP] = stack + 0x800;
        }
        p.cpu.gpr[RCX] = 0x1000;
        p.cpu.gpr[RDX] = format as u64;
        p.cpu.gpr[R8] = flags as u64;
        p.cpu.gpr[R9] = count;
        p.wr64(p.cpu.gpr[RSP] + 40, desc).unwrap();
    }

    #[test]
    fn immutable_output_queries_are_modeled_and_stateful_setters_unresolved() {
        let mut p = Process::new(Vfs::new());
        assert_eq!(method("IDXGIFactory", "IsCurrent").unwrap()(&mut p), Ok(1));
        assert_eq!(
            method("IDXGIOutput", "IsWindowedStereoEnabled").unwrap()(&mut p),
            Ok(0)
        );
        for method_name in [
            "MakeWindowAssociation",
            "SetFullscreenState",
            "ResizeTarget",
            "WaitForVBlank",
            "SetMaximumFrameLatency",
            "SetGPUThreadPriority",
            "SetResizeFlags",
        ] {
            assert!(
                method("IDXGISwapChain", method_name).is_none(),
                "{method_name} should remain visibly unsupported"
            );
        }
    }

    #[test]
    fn swap_chain_frame_latency_wait_handle_is_stable_and_ready() {
        let mut p = Process::new(Vfs::new());
        let swap_chain = create(
            &mut p,
            "IDXGISwapChain2",
            Kind::SwapChain {
                id: 17,
                backbuffer: 0,
            },
        );
        p.cpu.gpr[RCX] = swap_chain;

        let first = get_frame_latency_waitable_object(&mut p).unwrap();
        assert!(matches!(
            p.handles.get(&first),
            Some(Handle::Event {
                signaled: true,
                manual: true
            })
        ));
        assert_eq!(get_frame_latency_waitable_object(&mut p).unwrap(), first);
    }

    #[test]
    fn display_mode_list_validates_inputs_and_obeys_count_capacity() {
        let mut p = Process::new(Vfs::new());
        let mem = p.alloc_virtual(0x1000, anywasm_cpu::mem::PROT_RW);
        let count = mem;
        let desc = mem + 16;

        p.wr32(count, 0).unwrap();
        set_mode_list_args(&mut p, 28, 0, count, 0);
        assert_eq!(get_display_mode_list(&mut p), Ok(S_OK));
        assert_eq!(p.rd32(count), Ok(4));

        p.wr32(count, 2).unwrap();
        set_mode_list_args(&mut p, 28, 0, count, desc);
        assert_eq!(get_display_mode_list(&mut p), Ok(DXGI_ERROR_MORE_DATA));
        assert_eq!(p.rd32(count), Ok(2));
        assert_eq!(p.rd32(desc), Ok(1280));
        assert_eq!(p.rd32(desc + 28), Ok(1600));

        p.wr32(count, 4).unwrap();
        set_mode_list_args(&mut p, 87, 0, count, desc);
        assert_eq!(get_display_mode_list(&mut p), Ok(S_OK));
        assert_eq!(p.rd32(count), Ok(4));
        assert_eq!(p.rd32(desc + 16), Ok(87));

        set_mode_list_args(&mut p, 87, 0x4, count, desc);
        assert_eq!(get_display_mode_list(&mut p), Ok(DXGI_ERROR_INVALID_CALL));

        set_mode_list_args(&mut p, 999, 0, count, 0);
        assert_eq!(get_display_mode_list(&mut p), Ok(DXGI_ERROR_NOT_FOUND));
    }

    #[test]
    fn find_closest_mode_returns_a_mode_from_the_advertised_set() {
        let mut p = Process::new(Vfs::new());
        let mem = p.alloc_virtual(0x1000, anywasm_cpu::mem::PROT_RW);
        let requested = mem;
        let closest = mem + 64;
        for (i, value) in [1700u32, 950, 60, 1, 28, 0, 0].iter().enumerate() {
            p.wr32(requested + i as u64 * 4, *value).unwrap();
        }
        p.cpu.gpr[RCX] = 0x1000;
        p.cpu.gpr[RDX] = requested;
        p.cpu.gpr[R8] = closest;
        p.cpu.gpr[R9] = 0;

        assert_eq!(
            method("IDXGIOutput", "FindClosestMatchingMode").unwrap()(&mut p),
            Ok(S_OK)
        );
        let actual = (0..7)
            .map(|i| p.rd32(closest + i * 4).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, vec![1600, 900, 60, 1, 28, 0, 0]);

        p.wr32(requested + 16, 999).unwrap();
        assert_eq!(
            method("IDXGIOutput", "FindClosestMatchingMode").unwrap()(&mut p),
            Ok(DXGI_ERROR_NOT_FOUND)
        );

        p.wr32(requested + 16, 0).unwrap();
        let device = create(&mut p, "ID3D11Device", Kind::Device);
        p.cpu.gpr[R9] = device;
        assert_eq!(
            method("IDXGIOutput", "FindClosestMatchingMode").unwrap()(&mut p),
            Ok(S_OK)
        );
        assert_eq!(p.rd32(closest + 16), Ok(28));
    }

    #[test]
    fn present_serializes_sync_interval_and_rejects_values_above_four() {
        use anywasm_cpu::cpu::{R8, RCX, RDX};

        let mut p = Process::new(Vfs::new());
        let backbuffer = create(&mut p, "ID3D11Texture2D", Kind::Texture2D { id: 33 });
        let swap_chain = create(
            &mut p,
            "IDXGISwapChain1",
            Kind::SwapChain { id: 17, backbuffer },
        );
        p.cpu.gpr[RCX] = swap_chain;
        p.cpu.gpr[RDX] = 3;
        p.cpu.gpr[R8] = 0;

        assert_eq!(present(&mut p).unwrap(), S_OK);
        assert_eq!(
            super::super::gpu::decode(&p.extra.gpu.buf),
            vec![(
                op::PRESENT,
                [17u32, 33, 3]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect()
            )]
        );
        assert_eq!(p.yield_request, Some(Stop::Frame));

        p.extra.gpu.buf.clear();
        p.cpu.gpr[RDX] = 5;
        assert_eq!(present(&mut p).unwrap(), DXGI_ERROR_INVALID_CALL);
        assert!(p.extra.gpu.buf.is_empty());
    }
}

/// Build a swap chain object (and its backbuffer texture) for a window.
pub fn make_swap_chain(
    p: &mut Process,
    hwnd: u64,
    mut w: u32,
    mut h: u32,
    format: u32,
    buffers: u32,
) -> u64 {
    let (win_gpu, ww, wh) = match p.extra.win.windows.get(&hwnd) {
        Some(x) => (x.gpu_id, x.w as u32, x.h as u32),
        None => (0, 0, 0),
    };
    if w == 0 {
        w = if ww != 0 { ww } else { 1280 };
    }
    if h == 0 {
        h = if wh != 0 { wh } else { 720 };
    }
    let format = if format == 0 { 28 } else { format };
    let tex_id = new_res(
        p,
        ResInfo {
            width: w,
            height: h,
            mips: 1,
            array: 1,
            format,
            samples: 1,
            usage: 0,
            bind: 0x20 | 0x8,
            ..Default::default()
        },
    );
    let backbuffer = create(p, "ID3D11Texture2D", Kind::Texture2D { id: tex_id });
    let sc_id = p.extra.gpu.new_id();
    let sc = create(
        p,
        "IDXGISwapChain1",
        Kind::SwapChain {
            id: sc_id,
            backbuffer,
        },
    );
    p.extra.gpu.emit(
        op::SWAPCHAIN,
        Pk::new()
            .u32(sc_id)
            .u32(win_gpu)
            .u32(tex_id)
            .u32(w)
            .u32(h)
            .u32(format)
            .u32(buffers),
    );
    sc
}

fn create_swap_chain(p: &mut Process) -> R {
    // (this, IUnknown* pDevice, DXGI_SWAP_CHAIN_DESC* pDesc, IDXGISwapChain** ppSwapChain)
    let (desc, out) = (p.arg(2), p.arg(3));
    if desc == 0 || out == 0 {
        return Ok(E_INVALIDARG);
    }
    let (w, h, fmt) = (p.rd32(desc)?, p.rd32(desc + 4)?, p.rd32(desc + 16)?);
    let (buffers, hwnd) = (p.rd32(desc + 40)?, p.rd64(desc + 48)?);
    let sc = make_swap_chain(p, hwnd, w, h, fmt, buffers.max(1));
    p.wr64(out, sc)?;
    Ok(S_OK)
}

fn create_swap_chain_for_hwnd(p: &mut Process) -> R {
    // (this, pDevice, HWND, DXGI_SWAP_CHAIN_DESC1* pDesc, pFullscreenDesc, pRestrictToOutput, IDXGISwapChain1** pp)
    let (hwnd, desc, out) = (p.arg(2), p.arg(3), p.arg(6));
    if desc == 0 || out == 0 {
        return Ok(E_INVALIDARG);
    }
    let (w, h, fmt, buffers) = (
        p.rd32(desc)?,
        p.rd32(desc + 4)?,
        p.rd32(desc + 8)?,
        p.rd32(desc + 28)?,
    );
    let sc = make_swap_chain(p, hwnd, w, h, fmt, buffers.max(1));
    p.wr64(out, sc)?;
    Ok(S_OK)
}

fn present(p: &mut Process) -> R {
    let this = p.arg(0);
    let sync_interval = p.arg(1) as u32;
    if sync_interval > 4 {
        return Ok(DXGI_ERROR_INVALID_CALL);
    }
    let Some(Kind::SwapChain { id, backbuffer }) = kind_of(p, this) else {
        return Ok(DXGI_ERROR_INVALID_CALL);
    };
    let tex = match kind_of(p, backbuffer) {
        Some(Kind::Texture2D { id }) => id,
        _ => 0,
    };
    p.extra
        .gpu
        .emit(op::PRESENT, Pk::new().u32(id).u32(tex).u32(sync_interval));
    p.yield_request = Some(Stop::Frame);
    Ok(S_OK)
}

fn get_buffer(p: &mut Process) -> R {
    // (this, UINT Buffer, REFIID riid, void** ppSurface)
    let (this, idx, out) = (p.arg(0), p.arg(1), p.arg(3));
    let Some(Kind::SwapChain { backbuffer, .. }) = kind_of(p, this) else {
        return Ok(DXGI_ERROR_INVALID_CALL);
    };
    if idx != 0 {
        return Ok(DXGI_ERROR_INVALID_CALL);
    }
    p.extra.com.objs.get_mut(&backbuffer).map(|o| o.refs += 1);
    p.wr64(out, backbuffer)?;
    Ok(S_OK)
}

fn resize_buffers(p: &mut Process) -> R {
    // (this, BufferCount, Width, Height, NewFormat, Flags)
    let (this, w, h, fmt) = (p.arg(0), p.arg(2) as u32, p.arg(3) as u32, p.arg(4) as u32);
    let Some(Kind::SwapChain { id, backbuffer }) = kind_of(p, this) else {
        return Ok(DXGI_ERROR_INVALID_CALL);
    };
    let Some(Kind::Texture2D { id: tex }) = kind_of(p, backbuffer) else {
        return Ok(DXGI_ERROR_INVALID_CALL);
    };
    let (win_w, win_h) = p
        .extra
        .win
        .windows
        .get(&p.extra.win.main_window)
        .map(|x| (x.w as u32, x.h as u32))
        .unwrap_or((1280, 720));
    let (w, h) = (
        if w == 0 { win_w } else { w },
        if h == 0 { win_h } else { h },
    );
    if let Some(r) = p.extra.com.res.get_mut(&tex) {
        r.width = w;
        r.height = h;
        if fmt != 0 {
            r.format = fmt;
        }
    }
    let format = p.extra.com.res.get(&tex).map(|r| r.format).unwrap_or(28);
    p.extra.gpu.emit(
        op::RESIZE_SWAPCHAIN,
        Pk::new().u32(id).u32(tex).u32(w).u32(h).u32(format),
    );
    Ok(S_OK)
}
