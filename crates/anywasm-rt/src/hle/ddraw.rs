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

macro_rules! unimplemented_method {
    ($function:ident, $name:literal) => {
        fn $function(_p: &mut Process) -> R {
            Err(Stop::Unimplemented {
                dll: "ddraw".into(),
                name: format!("{} (DirectDraw operation is not modeled)", $name),
            })
        }
    };
}

unimplemented_method!(unimplemented_compact, "IDirectDraw7::Compact");
unimplemented_method!(unimplemented_flip_to_gdi, "IDirectDraw7::FlipToGDISurface");
unimplemented_method!(unimplemented_initialize, "IDirectDraw7::Initialize");
unimplemented_method!(
    unimplemented_restore_all_surfaces,
    "IDirectDraw7::RestoreAllSurfaces"
);
unimplemented_method!(
    unimplemented_test_cooperative_level,
    "IDirectDraw7::TestCooperativeLevel"
);
unimplemented_method!(unimplemented_get_caps, "IDirectDraw7::GetCaps");
unimplemented_method!(
    unimplemented_get_device_identifier,
    "IDirectDraw7::GetDeviceIdentifier"
);
unimplemented_method!(unimplemented_enum_surfaces, "IDirectDraw7::EnumSurfaces");
unimplemented_method!(unimplemented_evaluate_mode, "IDirectDraw7::EvaluateMode");
unimplemented_method!(
    unimplemented_set_cooperative_level,
    "IDirectDraw7::SetCooperativeLevel"
);
unimplemented_method!(
    unimplemented_wait_for_vblank,
    "IDirectDraw7::WaitForVerticalBlank"
);
unimplemented_method!(unimplemented_start_mode_test, "IDirectDraw7::StartModeTest");

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
        "Compact" => unimplemented_compact,
        "FlipToGDISurface" => unimplemented_flip_to_gdi,
        "Initialize" => unimplemented_initialize,
        "SetCooperativeLevel" => unimplemented_set_cooperative_level,
        "WaitForVerticalBlank" => unimplemented_wait_for_vblank,
        "StartModeTest" => unimplemented_start_mode_test,
        "RestoreAllSurfaces" => unimplemented_restore_all_surfaces,
        "TestCooperativeLevel" => unimplemented_test_cooperative_level,
        "RestoreDisplayMode" => restore_display_mode,
        "SetDisplayMode" => set_display_mode,
        "GetCaps" => unimplemented_get_caps,
        "GetDisplayMode" => get_display_mode,
        "GetFourCCCodes" => get_fourcc_codes,
        "GetGDISurface" => get_gdi_surface,
        "GetMonitorFrequency" => get_monitor_frequency,
        "GetScanLine" => get_scan_line,
        "GetVerticalBlankStatus" => get_vertical_blank_status,
        // There is no translated DirectDraw surface heap to report. Return the
        // API's unsupported HRESULT rather than fabricating a video-memory
        // budget. GTA San Andreas ignores this query's HRESULT.
        "GetAvailableVidMem" => get_available_vidmem,
        "GetSurfaceFromDC" => get_surface_from_dc,
        "GetDeviceIdentifier" => unimplemented_get_device_identifier,
        "EvaluateMode" => unimplemented_evaluate_mode,
        "EnumDisplayModes" => enum_display_modes,
        "EnumSurfaces" => unimplemented_enum_surfaces,
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
        0xc0, 0x5e, 0xe6, 0x15, 0x9c, 0x3b, 0xd2, 0x11, 0xb9, 0x2f, 0x00, 0x60, 0x97, 0x97, 0xea,
        0x5b,
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

fn write_display_mode(
    p: &mut Process,
    out: u64,
    width: u32,
    height: u32,
    bpp: u32,
) -> Result<bool, Stop> {
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
    Ok(
        if write_display_mode(p, p.arg(1), mode.0, mode.1, mode.2)? {
            S_OK
        } else {
            E_INVALIDARG
        },
    )
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

fn get_available_vidmem(_p: &mut Process) -> R {
    Ok(E_NOTIMPL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_without_directdraw_translation_report_coverage_gaps() {
        let mut process = Process::new(crate::vfs::Vfs::new());
        for method_name in [
            "SetCooperativeLevel",
            "WaitForVerticalBlank",
            "FlipToGDISurface",
        ] {
            let result = method("IDirectDraw7", method_name).unwrap()(&mut process);
            assert!(matches!(
                result,
                Err(Stop::Unimplemented { ref dll, ref name })
                    if dll == "ddraw" && name.contains(method_name)
            ));
        }
    }

    #[test]
    fn unbacked_directdraw_state_operations_are_coverage_gaps() {
        let mut process = Process::new(crate::vfs::Vfs::new());
        for method_name in [
            "RestoreAllSurfaces",
            "TestCooperativeLevel",
            "GetCaps",
            "GetDeviceIdentifier",
            "EnumSurfaces",
            "EvaluateMode",
        ] {
            let result = method("IDirectDraw7", method_name).unwrap()(&mut process);
            assert!(
                matches!(
                    result,
                    Err(Stop::Unimplemented { ref dll, ref name })
                        if dll == "ddraw" && name.contains(method_name)
                ),
                "{method_name} must not claim success without backing state"
            );
        }
    }

    #[test]
    fn available_video_memory_is_explicitly_unsupported_without_a_surface_heap() {
        let mut process = Process::new(crate::vfs::Vfs::new());
        let result = method("IDirectDraw7", "GetAvailableVidMem").unwrap()(&mut process);
        assert_eq!(result.unwrap(), E_NOTIMPL);
    }
}

fn get_surface_from_dc(p: &mut Process) -> R {
    let out = p.arg(2);
    if out != 0 {
        p.wrptr(out, 0)?;
    }
    Ok(E_NOTIMPL)
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
