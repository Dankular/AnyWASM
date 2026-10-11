//! Small virtual display device contexts used by windowing libraries during monitor discovery.

use super::*;
use crate::process::*;

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "CreateDCW" | "CreateICW" => create_display_dc,
        "CreateCompatibleDC" => |p| Ok(p.new_handle(Handle::Other)),
        "DeleteDC" => |p| Ok(p.handles.remove(&p.arg(0)).is_some() as u64),
        "GetDeviceCaps" => get_device_caps,
        "SaveDC" => |_| Ok(1),
        "RestoreDC" => |_| Ok(1),
        _ => return None,
    })
}

fn create_display_dc(p: &mut Process) -> R {
    let driver = p.arg(0);
    if driver != 0 {
        let driver_name = p.read_wstr(driver)?;
        if !driver_name.eq_ignore_ascii_case("DISPLAY") {
            p.set_last_error(1801); // ERROR_INVALID_PRINTER_NAME
            return Ok(0);
        }
    }
    Ok(p.new_handle(Handle::Other))
}

fn get_device_caps(p: &mut Process) -> R {
    let index = p.arg(1) as u32;
    Ok(match index {
        2 => 1,                    // TECHNOLOGY: DT_RASDISPLAY
        8 | 118 => 1920,           // HORZRES / DESKTOPHORZRES
        10 | 117 => 1080,          // VERTRES / DESKTOPVERTRES
        12 => 32,                  // BITSPIXEL
        14 => 1,                   // PLANES
        4 | 6 => 509,              // HORZSIZE / VERTSIZE (approx. 96 DPI)
        88 | 90 => 96,
        110 => 1920,               // PHYSICALWIDTH
        111 => 1080,               // PHYSICALHEIGHT
        116 => 60,                 // VREFRESH
        38 => 0x0001,              // RASTERCAPS: RC_BITBLT
        _ => 0,
    })
}
