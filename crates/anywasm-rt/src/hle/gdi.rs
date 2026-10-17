//! Small in-process GDI object model used by windowing libraries.

use super::*;
use crate::process::*;
use anywasm_cpu::mem::{PROT_R, PROT_RW};

const GDI_PAGE: u64 = 0x1000;
const GDI_DATA_OFFSET: u64 = GDI_PAGE;
const MAX_GDI_DATA: u64 = 256 * 1024 * 1024;
const GDI_MAGIC: u32 = 0x4744_4931; // "GDI1"

const KIND_DC: u32 = 1;
const KIND_BITMAP: u32 = 2;
const KIND_BRUSH: u32 = 3;

const DC_DISPLAY: u64 = 1;
const DC_INFORMATION: u64 = 2;
const DC_COMPATIBLE: u64 = 3;

const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NOT_SUPPORTED: u32 = 50;

#[derive(Clone, Copy)]
struct GdiResource {
    base: u64,
    allocation_size: u64,
    data: u64,
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "CreateDCA" => create_display_dc_a,
        "CreateICA" => create_information_dc_a,
        "CreateDCW" => create_display_dc_w,
        "CreateICW" => create_information_dc_w,
        "CreateCompatibleDC" => create_compatible_dc,
        "CreateBitmap" => create_bitmap,
        "CreateDIBSection" => create_dib_section,
        "CreateSolidBrush" => create_solid_brush,
        "DeleteDC" => delete_dc,
        "DeleteObject" => delete_object,
        "GetDeviceCaps" => get_device_caps,
        _ => return None,
    })
}

fn allocate_resource(
    p: &mut Process,
    kind: u32,
    data_size: u64,
    fields: [u64; 3],
) -> Result<u64, Stop> {
    let Some(request_size) = GDI_DATA_OFFSET.checked_add(data_size) else {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    };
    if data_size > MAX_GDI_DATA {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    }
    let Some(base) = p.try_alloc_virtual(request_size, PROT_RW) else {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    };
    let allocation_size = p.vmem.get(&base).map(|(size, _)| *size).unwrap_or(0);
    if allocation_size < request_size {
        p.mem.unmap(base, allocation_size);
        p.vmem.remove(&base);
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    }

    p.wr32(base, GDI_MAGIC)?;
    p.wr32(base + 4, kind)?;
    p.wr64(base + 8, allocation_size)?;
    p.wr64(base + 16, base + GDI_DATA_OFFSET)?;
    p.wr64(base + 24, data_size)?;
    p.wr64(base + 32, fields[0])?;
    p.wr64(base + 40, fields[1])?;
    p.wr64(base + 48, fields[2])?;

    if !p.mem.protect(base, GDI_PAGE, PROT_R) {
        p.mem.unmap(base, allocation_size);
        p.vmem.remove(&base);
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    }

    // Virtual allocations are page aligned, leaving the low two handle bits free
    // to distinguish GDI object kinds. The descriptor remains guest-readable.
    let handle = base | kind as u64;
    p.handles.insert(handle, Handle::Other);
    Ok(handle)
}

fn resource(p: &Process, handle: u64, expected_kind: u32) -> Option<GdiResource> {
    if handle == 0 || handle & 3 != expected_kind as u64 {
        return None;
    }
    if !matches!(p.handles.get(&handle), Some(Handle::Other)) {
        return None;
    }
    let base = handle & !3;
    let allocation_size = p.vmem.get(&base)?.0;
    let alloc_end = base.checked_add(allocation_size)?;
    let data = p.rd64(base + 16).ok()?;
    let data_size = p.rd64(base + 24).ok()?;
    let data_end = data.checked_add(data_size)?;
    if p.rd32(base).ok()? != GDI_MAGIC
        || p.rd32(base + 4).ok()? != expected_kind
        || p.rd64(base + 8).ok()? != allocation_size
        || data < base + GDI_DATA_OFFSET
        || data_end > alloc_end
    {
        return None;
    }
    Some(GdiResource {
        base,
        allocation_size,
        data,
    })
}

fn release_resource(p: &mut Process, handle: u64, expected_kind: u32) -> bool {
    let Some(object) = resource(p, handle, expected_kind) else {
        return false;
    };
    p.handles.remove(&handle);
    p.mem.unmap(object.base, object.allocation_size);
    p.vmem.remove(&object.base);
    true
}

fn display_dc(p: &mut Process, wide: bool, info: bool) -> R {
    let driver = p.arg(0);
    if driver != 0 {
        let display = if wide {
            p.read_wstr(driver)?.eq_ignore_ascii_case("DISPLAY")
        } else {
            p.read_cstr(driver)?.eq_ignore_ascii_case("DISPLAY")
        };
        if !display {
            p.set_last_error(1801); // ERROR_INVALID_PRINTER_NAME
            return Ok(0);
        }
    }
    allocate_resource(
        p,
        KIND_DC,
        0,
        [if info { DC_INFORMATION } else { DC_DISPLAY }, 0, 0],
    )
}

fn create_display_dc_a(p: &mut Process) -> R {
    display_dc(p, false, false)
}

fn create_information_dc_a(p: &mut Process) -> R {
    display_dc(p, false, true)
}

fn create_display_dc_w(p: &mut Process) -> R {
    display_dc(p, true, false)
}

fn create_information_dc_w(p: &mut Process) -> R {
    display_dc(p, true, true)
}

fn create_compatible_dc(p: &mut Process) -> R {
    let source = p.arg(0);
    if source != 0 && resource(p, source, KIND_DC).is_none() {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    allocate_resource(p, KIND_DC, 0, [DC_COMPATIBLE, source, 0])
}

fn delete_dc(p: &mut Process) -> R {
    let handle = p.arg(0);
    if release_resource(p, handle, KIND_DC) {
        Ok(1)
    } else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        Ok(0)
    }
}

fn delete_object(p: &mut Process) -> R {
    let handle = p.arg(0);
    for kind in [KIND_BITMAP, KIND_BRUSH] {
        if release_resource(p, handle, kind) {
            return Ok(1);
        }
    }
    p.set_last_error(ERROR_INVALID_HANDLE);
    Ok(0)
}

fn bitmap_data_size(width: u64, height: u64, bits_per_pixel: u64, dib: bool) -> Option<u64> {
    let row_bits = width.checked_mul(bits_per_pixel)?;
    let row_bytes = if dib {
        row_bits.checked_add(31)?.checked_div(32)?.checked_mul(4)?
    } else {
        row_bits.checked_add(15)?.checked_div(16)?.checked_mul(2)?
    };
    let size = row_bytes.checked_mul(height)?;
    (size <= MAX_GDI_DATA).then_some(size)
}

fn create_bitmap(p: &mut Process) -> R {
    let width = p.arg(0) as u32 as i32;
    let height = p.arg(1) as u32 as i32;
    let planes = p.arg(2) as u32;
    let bits_per_plane = p.arg(3) as u32;
    let bits_pointer = p.arg(4);
    if width <= 0 || height <= 0 || planes == 0 || bits_per_plane == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let Some(total_bpp) = planes.checked_mul(bits_per_plane) else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    if !matches!(total_bpp, 1 | 4 | 8 | 16 | 24 | 32) {
        p.set_last_error(ERROR_NOT_SUPPORTED);
        return Ok(0);
    }
    let Some(data_size) = bitmap_data_size(width as u64, height as u64, total_bpp as u64, false)
    else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    let initial_bits = if bits_pointer == 0 {
        None
    } else {
        Some(p.read_bytes(bits_pointer, data_size as usize)?)
    };
    let handle = allocate_resource(
        p,
        KIND_BITMAP,
        data_size,
        [
            width as u64,
            height as u64,
            ((planes as u64) << 32) | bits_per_plane as u64,
        ],
    )?;
    if handle != 0 {
        if let Some(bytes) = initial_bits {
            let object = resource(p, handle, KIND_BITMAP).expect("new GDI bitmap must validate");
            p.write_bytes(object.data, &bytes)?;
        }
    }
    Ok(handle)
}

fn create_dib_section(p: &mut Process) -> R {
    let hdc = p.arg(0);
    let info = p.arg(1);
    let usage = p.arg(2) as u32;
    let bits_out = p.arg(3);
    let section = p.arg(4);
    let offset = p.arg(5);

    if bits_out != 0 {
        p.wrptr(bits_out, 0)?;
    }
    if hdc == 0 || resource(p, hdc, KIND_DC).is_none() {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if info == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if usage != 0 || section != 0 || offset != 0 {
        p.set_last_error(ERROR_NOT_SUPPORTED);
        return Ok(0);
    }

    let header_size = p.rd32(info)?;
    if header_size != 40 {
        p.set_last_error(if header_size < 40 {
            ERROR_INVALID_PARAMETER
        } else {
            ERROR_NOT_SUPPORTED
        });
        return Ok(0);
    }
    let width = p.rd32(info + 4)? as i32;
    let signed_height = p.rd32(info + 8)? as i32;
    let planes = p.rd16(info + 12)? as u32;
    let bits_per_pixel = p.rd16(info + 14)? as u32;
    let compression = p.rd32(info + 16)?;
    let image_size = p.rd32(info + 20)? as u64;
    if width <= 0 || signed_height == 0 || planes != 1 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if !matches!(bits_per_pixel, 1 | 4 | 8 | 16 | 24 | 32) || compression != 0 {
        p.set_last_error(ERROR_NOT_SUPPORTED);
        return Ok(0);
    }
    let Some(required_size) = bitmap_data_size(
        width as u64,
        signed_height.unsigned_abs() as u64,
        bits_per_pixel as u64,
        true,
    ) else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    let data_size = if image_size == 0 {
        required_size
    } else if image_size >= required_size && image_size <= MAX_GDI_DATA {
        image_size
    } else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    let handle = allocate_resource(
        p,
        KIND_BITMAP,
        data_size,
        [
            width as u64,
            signed_height as i64 as u64,
            bits_per_pixel as u64,
        ],
    )?;
    if handle != 0 && bits_out != 0 {
        let object = resource(p, handle, KIND_BITMAP).expect("new DIB section must validate");
        p.wrptr(bits_out, object.data)?;
    }
    Ok(handle)
}

fn create_solid_brush(p: &mut Process) -> R {
    let color = p.arg(0) & 0x00ff_ffff;
    allocate_resource(p, KIND_BRUSH, 0, [color, 0, 0])
}

fn get_device_caps(p: &mut Process) -> R {
    let dc = p.arg(0);
    if resource(p, dc, KIND_DC).is_none() {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    let index = p.arg(1) as u32;
    Ok(match index {
        2 => 1,           // TECHNOLOGY: DT_RASDISPLAY
        8 | 118 => 1920,  // HORZRES / DESKTOPHORZRES
        10 | 117 => 1080, // VERTRES / DESKTOPVERTRES
        12 => 32,         // BITSPIXEL
        14 => 1,          // PLANES
        4 | 6 => 509,     // HORZSIZE / VERTSIZE (approx. 96 DPI)
        88 | 90 => 96,
        110 => 1920,  // PHYSICALWIDTH
        111 => 1080,  // PHYSICALHEIGHT
        116 => 60,    // VREFRESH
        38 => 0x0001, // RASTERCAPS: RC_BITBLT
        _ => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::{lookup, GDI_DATA_OFFSET};
    use crate::{process::Process, vfs::Vfs};
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::PROT_RW;

    fn set_args(p: &mut Process, args: &[u32]) {
        p.cpu.mode = CpuMode::X86_32;
        let stack = p.alloc_virtual(0x1000, PROT_RW);
        p.cpu.gpr[RSP] = stack;
        p.wr32(stack, 0xfeed_face).unwrap();
        for (i, arg) in args.iter().copied().enumerate() {
            p.wr32(stack + 4 + i as u64 * 4, arg).unwrap();
        }
    }

    fn process_with_args(args: &[u32]) -> Process {
        let mut p = Process::new(Vfs::new());
        p.vmem_next = 0x2000_0000;
        set_args(&mut p, args);
        p
    }

    fn call(p: &mut Process, name: &str) -> u64 {
        lookup(name).unwrap_or_else(|| panic!("gdi32!{name} should resolve"))(p).unwrap()
    }

    #[test]
    fn unsupported_dc_state_stack_apis_are_not_advertised() {
        assert!(lookup("SaveDC").is_none());
        assert!(lookup("RestoreDC").is_none());
    }

    fn alloc_cstr(p: &mut Process, value: &str) -> u64 {
        let address = p.alloc_virtual(0x1000, PROT_RW);
        p.write_cstr(address, value).unwrap();
        address
    }

    #[test]
    fn ansi_display_dc_apis_create_deletable_device_contexts() {
        for name in ["CreateDCA", "CreateICA"] {
            let mut p = process_with_args(&[0, 0, 0, 0]);
            let driver = alloc_cstr(&mut p, "DISPLAY");
            let stack = p.cpu.gpr[RSP];
            p.wr32(stack + 4, driver as u32).unwrap();

            let dc = call(&mut p, name);
            assert_ne!(dc, 0, "{name} should create a display DC");
            assert!(p.handles.contains_key(&dc));

            set_args(&mut p, &[dc as u32, 8]);
            assert_eq!(call(&mut p, "GetDeviceCaps"), 1920);

            set_args(&mut p, &[dc as u32]);
            assert_eq!(call(&mut p, "DeleteDC"), 1);
            assert!(!p.handles.contains_key(&dc));
        }
    }

    #[test]
    fn compatible_dc_requires_a_live_device_context_and_has_its_own_lifetime() {
        let mut p = process_with_args(&[0]);
        let default_dc = call(&mut p, "CreateCompatibleDC");
        assert_ne!(default_dc, 0, "NULL source creates a memory DC");

        set_args(&mut p, &[0x1234_5678]);
        assert_eq!(call(&mut p, "CreateCompatibleDC"), 0);
        set_args(&mut p, &[default_dc as u32]);
        assert_eq!(call(&mut p, "DeleteDC"), 1);

        set_args(&mut p, &[0, 0, 0, 0]);
        let display_dc = call(&mut p, "CreateDCA");
        set_args(&mut p, &[display_dc as u32]);
        let compatible_dc = call(&mut p, "CreateCompatibleDC");
        assert_ne!(compatible_dc, 0);

        set_args(&mut p, &[compatible_dc as u32]);
        assert_eq!(call(&mut p, "DeleteDC"), 1);
        set_args(&mut p, &[display_dc as u32]);
        assert_eq!(call(&mut p, "DeleteDC"), 1);
    }

    #[test]
    fn create_bitmap_owns_copied_bits_until_delete_object() {
        let mut p = process_with_args(&[2, 2, 1, 24, 0]);
        let bits = p.alloc_virtual(0x1000, PROT_RW);
        p.write_bytes(bits, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])
            .unwrap();
        let stack = p.cpu.gpr[RSP];
        p.wr32(stack + 4 + 4 * 4, bits as u32).unwrap();

        let bitmap = call(&mut p, "CreateBitmap");
        assert_ne!(bitmap, 0);
        assert!(p.handles.contains_key(&bitmap));
        let bitmap_bits = (bitmap & !3) + GDI_DATA_OFFSET;
        assert_eq!(
            p.read_bytes(bitmap_bits, 12).unwrap(),
            (1u8..=12).collect::<Vec<_>>()
        );
        set_args(&mut p, &[bitmap as u32]);
        assert_eq!(call(&mut p, "DeleteObject"), 1);
        assert!(!p.handles.contains_key(&bitmap));
        assert!(p.rd8(bitmap_bits).is_err());
    }

    #[test]
    fn dib_section_returns_writable_bits_and_delete_object_releases_them() {
        let mut p = process_with_args(&[0, 0, 0, 0]);
        let display_dc = call(&mut p, "CreateDCA");
        let info = p.alloc_virtual(0x1000, PROT_RW);
        p.wr32(info, 40).unwrap();
        p.wr32(info + 4, 3).unwrap();
        p.wr32(info + 8, 2).unwrap();
        p.wr16(info + 12, 1).unwrap();
        p.wr16(info + 14, 24).unwrap();
        p.wr32(info + 16, 0).unwrap();
        let bits_out = p.alloc_virtual(0x1000, PROT_RW);
        set_args(
            &mut p,
            &[display_dc as u32, info as u32, 0, bits_out as u32, 0, 0],
        );

        let bitmap = call(&mut p, "CreateDIBSection");
        assert_ne!(bitmap, 0);
        let bits = p.rd32(bits_out).unwrap() as u64;
        assert_ne!(bits, 0);
        p.wr8(bits, 0x5a).unwrap();
        assert_eq!(p.rd8(bits).unwrap(), 0x5a);

        set_args(&mut p, &[bitmap as u32]);
        assert_eq!(call(&mut p, "DeleteObject"), 1);
        assert!(!p.handles.contains_key(&bitmap));
        assert!(p.rd8(bits).is_err());
    }

    #[test]
    fn solid_brush_is_a_deletable_gdi_object() {
        let mut p = process_with_args(&[0x0012_3456]);
        let brush = call(&mut p, "CreateSolidBrush");
        assert_ne!(brush, 0);
        assert!(p.handles.contains_key(&brush));
        assert_eq!(p.rd64((brush & !3) + 32).unwrap(), 0x0012_3456);
        set_args(&mut p, &[brush as u32]);
        assert_eq!(call(&mut p, "DeleteObject"), 1);
        assert!(!p.handles.contains_key(&brush));
        set_args(&mut p, &[brush as u32]);
        assert_eq!(call(&mut p, "DeleteObject"), 0);
    }
}
