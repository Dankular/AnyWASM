//! Narrow DirectDraw 7 compatibility for legacy Win32 startup and display probing.
//!
//! This currently exposes a display adapter and the IDirectDraw7 methods used by the
//! selected GTA San Andreas startup path. It does not translate DirectDraw surfaces or
//! Direct3D 7 rendering into the browser GPU stream.

use super::com::{self, Kind, S_OK};
use super::*;
use crate::process::{Process, Stop};

const E_NOTIMPL: u64 = 0x8000_4001;
const E_INVALIDARG: u64 = 0x8007_0057;
const DDSURFACEDESC2_SIZE: u32 = 124;
const DDPIXELFORMAT_OFFSET: u64 = 72;
const DESKTOP_MODE: (u32, u32, u32) = (1920, 1080, 32);
const AVAILABLE_VIDEO_MEMORY: u32 = 512 * 1024 * 1024;

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "DirectDrawCreateEx" => create_ex,
        _ => return None,
    })
}

pub fn method(iface: &str, name: &str) -> Option<HleFn> {
    if iface != "IDirectDraw7" {
        return None;
    }
    Some(match name {
        "Compact"
        | "FlipToGDISurface"
        | "Initialize"
        | "SetCooperativeLevel"
        | "WaitForVerticalBlank"
        | "RestoreAllSurfaces"
        | "TestCooperativeLevel"
        | "StartModeTest" => |_| Ok(S_OK),
        "RestoreDisplayMode" => restore_display_mode,
        "SetDisplayMode" => set_display_mode,
        "GetCaps" => get_caps,
        "GetDisplayMode" => get_display_mode,
        "GetFourCCCodes" => get_fourcc_codes,
        "GetGDISurface" => get_gdi_surface,
        "GetMonitorFrequency" => get_monitor_frequency,
        "GetScanLine" => get_scan_line,
        "GetVerticalBlankStatus" => get_vertical_blank_status,
        "GetAvailableVidMem" => get_available_vidmem,
        "GetSurfaceFromDC" => get_surface_from_dc,
        "GetDeviceIdentifier" => get_device_identifier,
        "EvaluateMode" => evaluate_mode,
        "EnumDisplayModes" => enum_display_modes,
        "EnumSurfaces" => |_| Ok(S_OK),
        "CreateClipper" | "CreatePalette" | "CreateSurface" | "DuplicateSurface" => {
            unsupported_object_creation
        }
        _ => return None,
    })
}

fn create_ex(p: &mut Process) -> R {
    let (out, iid, outer) = (p.arg(1), p.arg(2), p.arg(3));
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wrptr(out, 0)?;
    if outer != 0 || iid == 0 {
        return Ok(E_INVALIDARG);
    }
    let iid = p.read_bytes(iid, 16)?;
    const IID_IDIRECTDRAW7: [u8; 16] = [
        0xc0, 0x5e, 0xe6, 0x15, 0x9c, 0x3b, 0xd2, 0x11, 0xb9, 0x2f, 0x00, 0x60, 0x97, 0x97,
        0xea, 0x5b,
    ];
    if iid.as_slice() != IID_IDIRECTDRAW7 {
        return Ok(E_INVALIDARG);
    }
    let object = com::create(p, "IDirectDraw7", Kind::DirectDraw);
    p.wrptr(out, object)?;
    Ok(S_OK)
}

fn zero_struct(p: &mut Process, out: u64, minimum: u32) -> Result<bool, Stop> {
    if out == 0 {
        return Ok(false);
    }
    let size = p.rd32(out)?;
    if size < minimum || size > 4096 {
        return Ok(false);
    }
    p.write_bytes(out, &vec![0; size as usize])?;
    p.wr32(out, size)?;
    Ok(true)
}

fn get_caps(p: &mut Process) -> R {
    let (driver, hel) = (p.arg(1), p.arg(2));
    if (driver == 0 && hel == 0)
        || (driver != 0 && !zero_struct(p, driver, 4)?)
        || (hel != 0 && !zero_struct(p, hel, 4)?)
    {
        return Ok(E_INVALIDARG);
    }
    Ok(S_OK)
}

fn write_display_mode(p: &mut Process, out: u64, width: u32, height: u32, bpp: u32) -> Result<bool, Stop> {
    if !zero_struct(p, out, DDSURFACEDESC2_SIZE)? {
        return Ok(false);
    }
    p.wr32(out + 4, 0x0000_1006)?; // DDSD_HEIGHT | DDSD_WIDTH | DDSD_PIXELFORMAT
    p.wr32(out + 8, height)?;
    p.wr32(out + 12, width)?;
    p.wr32(out + 16, width.saturating_mul(bpp) / 8)?;
    p.wr32(out + 24, 60)?;
    p.wr32(out + DDPIXELFORMAT_OFFSET, 32)?;
    p.wr32(out + DDPIXELFORMAT_OFFSET + 4, 0x40)?; // DDPF_RGB
    p.wr32(out + DDPIXELFORMAT_OFFSET + 12, bpp)?;
    if bpp == 32 {
        p.wr32(out + DDPIXELFORMAT_OFFSET + 16, 0x00ff_0000)?;
        p.wr32(out + DDPIXELFORMAT_OFFSET + 20, 0x0000_ff00)?;
        p.wr32(out + DDPIXELFORMAT_OFFSET + 24, 0x0000_00ff)?;
        p.wr32(out + DDPIXELFORMAT_OFFSET + 28, 0xff00_0000)?;
    }
    Ok(true)
}

fn get_display_mode(p: &mut Process) -> R {
    let mode = p.extra.ddraw_mode.unwrap_or(DESKTOP_MODE);
    Ok(if write_display_mode(p, p.arg(1), mode.0, mode.1, mode.2)? {
        S_OK
    } else {
        E_INVALIDARG
    })
}

fn set_display_mode(p: &mut Process) -> R {
    let mode = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3) as u32);
    if mode.0 == 0 || mode.1 == 0 || !matches!(mode.2, 8 | 16 | 24 | 32) {
        return Ok(E_INVALIDARG);
    }
    p.extra.ddraw_mode = Some(mode);
    Ok(S_OK)
}

fn restore_display_mode(p: &mut Process) -> R {
    p.extra.ddraw_mode = None;
    Ok(S_OK)
}

fn get_fourcc_codes(p: &mut Process) -> R {
    let (count, codes) = (p.arg(1), p.arg(2));
    if count == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wr32(count, 0)?;
    if codes != 0 {
        p.wr32(codes, 0)?;
    }
    Ok(S_OK)
}

fn get_gdi_surface(p: &mut Process) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wrptr(out, 0)?;
    Ok(E_NOTIMPL)
}

fn get_monitor_frequency(p: &mut Process) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wr32(out, 60)?;
    Ok(S_OK)
}

fn get_scan_line(p: &mut Process) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wr32(out, 0)?;
    Ok(S_OK)
}

fn get_vertical_blank_status(p: &mut Process) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wr32(out, 0)?;
    Ok(S_OK)
}

fn get_available_vidmem(p: &mut Process) -> R {
    let (total, free) = (p.arg(2), p.arg(3));
    if total != 0 {
        p.wr32(total, AVAILABLE_VIDEO_MEMORY)?;
    }
    if free != 0 {
        p.wr32(free, AVAILABLE_VIDEO_MEMORY)?;
    }
    Ok(S_OK)
}

fn get_surface_from_dc(p: &mut Process) -> R {
    let out = p.arg(2);
    if out != 0 {
        p.wrptr(out, 0)?;
    }
    Ok(E_NOTIMPL)
}

fn get_device_identifier(p: &mut Process) -> R {
    let out = p.arg(1);
    if !zero_struct(p, out, 4)? {
        return Ok(E_INVALIDARG);
    }
    Ok(S_OK)
}

fn evaluate_mode(p: &mut Process) -> R {
    let out = p.arg(2);
    if out != 0 {
        p.wr32(out, 0)?;
    }
    Ok(S_OK)
}

fn enum_display_modes(p: &mut Process) -> R {
    let (context, callback) = (p.arg(3), p.arg(4));
    if callback == 0 {
        return Ok(E_INVALIDARG);
    }
    let descriptor = super::kernel32::alloc_block(p, DDSURFACEDESC2_SIZE as u64, true);
    for (width, height, bpp) in [
        (640, 480, 32),
        (800, 600, 32),
        (1024, 768, 32),
        (1280, 720, 32),
        DESKTOP_MODE,
    ] {
        write_display_mode(p, descriptor, width, height, bpp)?;
        let result = p.call_guest(callback, &[descriptor, context])?;
        if result == 0 {
            break;
        }
    }
    super::kernel32::free_block(p, descriptor);
    Ok(S_OK)
}

fn unsupported_object_creation(_p: &mut Process) -> R {
    Ok(E_NOTIMPL)
}
