//! Automation string and VARIANT operations used by 32-bit PE guests.
//!
//! BSTRs use the native Automation layout: a 32-bit byte length immediately before the UTF-16
//! payload and a trailing UTF-16 NUL. All allocated data lives in the guest process heap.

use crate::process::{HleFn, Process, Stop};

type R = Result<u64, Stop>;

const VT_EMPTY: u16 = 0;
const VT_NULL: u16 = 1;
const VT_I2: u16 = 2;
const VT_I4: u16 = 3;
const VT_R4: u16 = 4;
const VT_R8: u16 = 5;
const VT_CY: u16 = 6;
const VT_DATE: u16 = 7;
const VT_BSTR: u16 = 8;
const VT_DISPATCH: u16 = 9;
const VT_ERROR: u16 = 10;
const VT_BOOL: u16 = 11;
const VT_UNKNOWN: u16 = 13;
const VT_DECIMAL: u16 = 14;
const VT_I1: u16 = 16;
const VT_UI1: u16 = 17;
const VT_UI2: u16 = 18;
const VT_UI4: u16 = 19;
const VT_I8: u16 = 20;
const VT_UI8: u16 = 21;
const VT_INT: u16 = 22;
const VT_UINT: u16 = 23;
const VT_HRESULT: u16 = 25;
const VT_RECORD: u16 = 36;
const VT_INT_PTR: u16 = 37;
const VT_UINT_PTR: u16 = 38;
const VT_ARRAY: u16 = 0x2000;
const VT_BYREF: u16 = 0x4000;

const S_OK: u64 = 0;
const E_NOTIMPL: u64 = 0x8000_4001;

/// Resolve Sims 3's ordinal imports against the stable OLEAUT32 export ordinals.
///
/// The Windows 32-bit OLEAUT32 export table maps #4 to SysAllocStringLen, #6 to SysFreeString,
/// #7 to SysStringLen, #8 to VariantInit, and #9 to VariantClear.
pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "#4" | "SysAllocStringLen" => sys_alloc_string_len,
        "#6" | "SysFreeString" => sys_free_string,
        "#7" | "SysStringLen" => sys_string_len,
        "#8" | "VariantInit" => variant_init,
        "#9" | "VariantClear" => variant_clear,
        _ => return None,
    })
}

/// Number of x86 stdcall argument bytes consumed by the imported Automation procedures.
pub fn stdcall_stack_bytes(name: &str) -> Option<u64> {
    match name {
        "#4" | "SysAllocStringLen" => Some(8),
        "#6" | "SysFreeString" | "#7" | "SysStringLen" | "#8" | "VariantInit" | "#9"
        | "VariantClear" => Some(4),
        _ => None,
    }
}

/// SysAllocStringLen(src, characters): allocate a counted UTF-16 BSTR.
fn sys_alloc_string_len(p: &mut Process) -> R {
    let source = p.arg(0);
    let characters = p.arg(1) as u32;
    let byte_len = (characters as u64) * 2;
    let allocation_size = byte_len
        .checked_add(6)
        .ok_or_else(|| Stop::Crash("oleaut32!SysAllocStringLen length overflow".to_owned()))?;

    // Keep pathological lengths from making the host allocate multiple gigabytes for one guest
    // request. As with a real allocator under memory pressure, report allocation failure (NULL).
    if allocation_size > 256 * 1024 * 1024 {
        return Ok(0);
    }
    let base = crate::hle::kernel32::alloc_block(p, allocation_size, false);
    if base == 0 {
        return Ok(0);
    }

    p.wr32(base, byte_len as u32)?;
    let bstr = base + 4;
    if source != 0 && byte_len != 0 {
        let contents = p.read_bytes(source, byte_len as usize)?;
        p.write_bytes(bstr, &contents)?;
    } else if byte_len != 0 {
        // A NULL source means a zero-filled string of the requested length, not an empty BSTR.
        const ZERO_CHUNK: usize = 64 * 1024;
        let mut left = byte_len as usize;
        let mut cursor = bstr;
        let zeros = [0u8; ZERO_CHUNK];
        while left != 0 {
            let count = left.min(ZERO_CHUNK);
            p.write_bytes(cursor, &zeros[..count])?;
            cursor += count as u64;
            left -= count;
        }
    }
    p.wr16(bstr + byte_len, 0)?;
    Ok(bstr)
}

/// Free a BSTR allocated in the process heap. A BSTR's length prefix is its allocation base.
fn free_bstr(p: &mut Process, bstr: u64) -> Result<(), Stop> {
    if bstr == 0 {
        return Ok(());
    }
    let Some(base) = bstr.checked_sub(4) else {
        return Err(Stop::Crash(
            "oleaut32!SysFreeString received a BSTR below address 4".to_owned(),
        ));
    };
    let Some(allocation_size) = p.heap.size_of(base) else {
        return Err(Stop::Crash(format!(
            "oleaut32!SysFreeString cannot release non-process-heap BSTR {bstr:#x}"
        )));
    };
    let byte_len = p.rd32(base)? as u64;
    let required = byte_len.checked_add(6).ok_or_else(|| {
        Stop::Crash(format!(
            "oleaut32!SysFreeString has invalid BSTR length at {bstr:#x}"
        ))
    })?;
    if byte_len & 1 != 0 || required > allocation_size || p.rd16(bstr + byte_len)? != 0 {
        return Err(Stop::Crash(format!(
            "oleaut32!SysFreeString rejected invalid BSTR at {bstr:#x}"
        )));
    }
    if p.heap.free(base).is_none() {
        return Err(Stop::Crash(format!(
            "oleaut32!SysFreeString could not free BSTR {bstr:#x}"
        )));
    }
    Ok(())
}

/// SysFreeString(BSTR): NULL is explicitly a no-op.
fn sys_free_string(p: &mut Process) -> R {
    free_bstr(p, p.arg(0))?;
    Ok(0)
}

/// SysStringLen(BSTR): read the byte-length prefix and return its UTF-16 code-unit count.
fn sys_string_len(p: &mut Process) -> R {
    let bstr = p.arg(0);
    if bstr == 0 {
        return Ok(0);
    }
    Ok((p.rd32(bstr - 4)? / 2) as u64)
}

/// VariantInit(VARIANTARG *): initialize the complete 16-byte x86 VARIANT to VT_EMPTY.
fn variant_init(p: &mut Process) -> R {
    p.write_bytes(p.arg(0), &[0; 16])?;
    Ok(0)
}

fn has_no_owned_resources(vt: u16) -> bool {
    matches!(
        vt,
        VT_EMPTY
            | VT_NULL
            | VT_I2
            | VT_I4
            | VT_R4
            | VT_R8
            | VT_CY
            | VT_DATE
            | VT_ERROR
            | VT_BOOL
            | VT_DECIMAL
            | VT_I1
            | VT_UI1
            | VT_UI2
            | VT_UI4
            | VT_I8
            | VT_UI8
            | VT_INT
            | VT_UINT
            | VT_HRESULT
            | VT_INT_PTR
            | VT_UINT_PTR
    )
}

/// VariantClear(VARIANTARG *): release supported owned data, then set the value to VT_EMPTY.
///
/// Scalar values and BYREF variants have no ownership to release. BSTR values are freed through
/// the guest process heap. SAFEARRAYs and interface/record values can execute destructors or
/// guest vtables, so non-null instances fail explicitly and remain unchanged until that ownership
/// machinery is implemented.
fn variant_clear(p: &mut Process) -> R {
    let variant = p.arg(0);
    let vt = p.rd16(variant)?;
    if vt & VT_BYREF != 0 {
        p.write_bytes(variant, &[0; 16])?;
        return Ok(S_OK);
    }

    if vt & VT_ARRAY != 0 {
        if p.rd32(variant + 8)? != 0 {
            return Ok(E_NOTIMPL);
        }
        p.write_bytes(variant, &[0; 16])?;
        return Ok(S_OK);
    }

    match vt {
        VT_BSTR => free_bstr(p, p.rd32(variant + 8)? as u64)?,
        VT_UNKNOWN | VT_DISPATCH if p.rd32(variant + 8)? != 0 => return Ok(E_NOTIMPL),
        VT_RECORD => return Ok(E_NOTIMPL),
        _ if has_no_owned_resources(vt) => {}
        _ => return Ok(E_NOTIMPL),
    }

    p.write_bytes(variant, &[0; 16])?;
    Ok(S_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x10000;
    const STACK: u64 = BASE + 0x800;
    const SRC: u64 = BASE + 0x100;
    const VARIANT: u64 = BASE + 0x200;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p
    }

    fn call(p: &mut Process, name: &str, args: &[u32]) -> u64 {
        for (index, value) in args.iter().enumerate() {
            p.wr32(STACK + 4 + index as u64 * 4, *value).unwrap();
        }
        lookup(name).expect("OLEAUT32 ordinal handler is registered")(p).unwrap()
    }

    #[test]
    fn exact_sims_ordinals_resolve_to_the_documented_exports() {
        assert!(std::ptr::fn_addr_eq(
            lookup("#4").unwrap(),
            sys_alloc_string_len as HleFn
        ));
        assert!(std::ptr::fn_addr_eq(
            lookup("#6").unwrap(),
            sys_free_string as HleFn
        ));
        assert!(std::ptr::fn_addr_eq(
            lookup("#7").unwrap(),
            sys_string_len as HleFn
        ));
        assert!(std::ptr::fn_addr_eq(
            lookup("#8").unwrap(),
            variant_init as HleFn
        ));
        assert!(std::ptr::fn_addr_eq(
            lookup("#9").unwrap(),
            variant_clear as HleFn
        ));
        assert!(lookup("#5").is_none());
    }

    #[test]
    fn alloc_string_len_preserves_counted_utf16_data_and_prefix() {
        let mut p = process();
        p.write_bytes(SRC, &[b'A', 0, 0, 0, b'B', 0]).unwrap();
        let bstr = call(&mut p, "#4", &[SRC as u32, 3]) as u32 as u64;
        assert_ne!(bstr, 0);
        assert_eq!(p.rd32(bstr - 4).unwrap(), 6);
        assert_eq!(
            p.read_bytes(bstr, 8).unwrap(),
            [b'A', 0, 0, 0, b'B', 0, 0, 0]
        );
        assert_eq!(call(&mut p, "#7", &[bstr as u32]), 3);
    }

    #[test]
    fn alloc_string_len_with_null_source_creates_zeroed_string() {
        let mut p = process();
        let bstr = call(&mut p, "SysAllocStringLen", &[0, 2]) as u32 as u64;
        assert_ne!(bstr, 0);
        assert_eq!(p.rd32(bstr - 4).unwrap(), 4);
        assert_eq!(p.read_bytes(bstr, 6).unwrap(), [0; 6]);
    }

    #[test]
    fn free_string_releases_its_guest_heap_block_and_null_is_safe() {
        let mut p = process();
        let bstr = call(&mut p, "#4", &[SRC as u32, 0]) as u32 as u64;
        assert_ne!(bstr, 0);
        let base = bstr - 4;
        assert!(p.heap.size_of(base).is_some());
        assert_eq!(call(&mut p, "#6", &[bstr as u32]), 0);
        assert!(p.heap.size_of(base).is_none());
        assert_eq!(call(&mut p, "#6", &[0]), 0);
    }

    #[test]
    fn variant_init_clears_all_sixteen_bytes() {
        let mut p = process();
        p.write_bytes(VARIANT, &[0xaa; 16]).unwrap();
        assert_eq!(call(&mut p, "#8", &[VARIANT as u32]), 0);
        assert_eq!(p.read_bytes(VARIANT, 16).unwrap(), [0; 16]);
    }

    #[test]
    fn variant_clear_frees_bstr_and_clears_scalar_variants() {
        let mut p = process();
        let bstr = call(&mut p, "#4", &[SRC as u32, 1]) as u32 as u64;
        let base = bstr - 4;
        p.wr16(VARIANT, VT_BSTR).unwrap();
        p.wr32(VARIANT + 8, bstr as u32).unwrap();
        assert_eq!(call(&mut p, "#9", &[VARIANT as u32]), S_OK);
        assert!(p.heap.size_of(base).is_none());
        assert_eq!(p.read_bytes(VARIANT, 16).unwrap(), [0; 16]);

        p.wr16(VARIANT, VT_I4).unwrap();
        p.wr32(VARIANT + 8, 0x1234_5678).unwrap();
        assert_eq!(call(&mut p, "#9", &[VARIANT as u32]), S_OK);
        assert_eq!(p.read_bytes(VARIANT, 16).unwrap(), [0; 16]);
    }

    #[test]
    fn variant_clear_does_not_release_byref_and_fails_closed_for_owned_arrays() {
        let mut p = process();
        let bstr = call(&mut p, "#4", &[SRC as u32, 1]) as u32 as u64;
        let base = bstr - 4;
        p.wr16(VARIANT, VT_BSTR | VT_BYREF).unwrap();
        p.wr32(VARIANT + 8, bstr as u32).unwrap();
        assert_eq!(call(&mut p, "#9", &[VARIANT as u32]), S_OK);
        assert!(p.heap.size_of(base).is_some());

        p.wr16(VARIANT, VT_ARRAY | VT_I4).unwrap();
        p.wr32(VARIANT + 8, 0x1234_5678).unwrap();
        assert_eq!(call(&mut p, "#9", &[VARIANT as u32]), E_NOTIMPL);
        assert_eq!(p.rd16(VARIANT).unwrap(), VT_ARRAY | VT_I4);
        assert_eq!(p.rd32(VARIANT + 8).unwrap(), 0x1234_5678);
    }
}
