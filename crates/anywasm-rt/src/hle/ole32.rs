//! OLE initialization and basic COM activation status.

use super::*;
use crate::process::Process;

const S_OK: u64 = 0;
const S_FALSE: u64 = 1;
const REGDB_E_CLASSNOTREG: u64 = 0x8004_0154;
const CLASS_E_NOAGGREGATION: u64 = 0x8004_0110;
const E_NOINTERFACE: u64 = 0x8000_4002;
const E_POINTER: u64 = 0x8000_4003;
const E_INVALIDARG: u64 = 0x8007_0057;
const RPC_E_CHANGED_MODE: u64 = 0x8001_0106;
const COINIT_APARTMENTTHREADED: u32 = 0x2;
const COINIT_VALID_FLAGS: u32 = 0xe;
const CLSCTX_INPROC_SERVER: u32 = 0x1;

const CLSID_DIRECTSOUND8: [u8; 16] = [
    0x3f, 0xcc, 0x01, 0x39, 0xb5, 0x84, 0xa4, 0x4f, 0xba, 0x35, 0xaa, 0x81, 0x72, 0xb8, 0xa0, 0x9b,
];
const IID_IDIRECTSOUND8: [u8; 16] = [
    0x93, 0x7e, 0x0a, 0xc5, 0x95, 0xf3, 0x34, 0x48, 0x9e, 0xf6, 0x7f, 0xa9, 0x9d, 0xe5, 0x09, 0x66,
];

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "CoInitialize" => co_initialize,
        "CoInitializeEx" => co_initialize_ex,
        "CoUninitialize" => co_uninitialize,
        "CoTaskMemAlloc" => co_task_mem_alloc,
        "CoTaskMemRealloc" => co_task_mem_realloc,
        "CoTaskMemFree" => co_task_mem_free,
        "CoCreateGuid" => co_create_guid,
        "IIDFromString" => iid_from_string,
        "StringFromGUID2" => string_from_guid2,
        "CoCreateInstance" => co_create_instance,
        "CoGetClassObject" => co_get_class_object,
        _ => return None,
    })
}

fn co_initialize(p: &mut Process) -> R {
    if p.arg(0) != 0 {
        return Ok(E_INVALIDARG);
    }
    initialize_com(p, COINIT_APARTMENTTHREADED)
}

fn co_initialize_ex(p: &mut Process) -> R {
    if p.arg(0) != 0 {
        return Ok(E_INVALIDARG);
    }
    initialize_com(p, p.arg(1) as u32)
}

fn initialize_com(p: &mut Process, flags: u32) -> R {
    // Bit 0 and bits outside the documented COINIT flags are invalid. The apartment model is
    // selected by bit 1; the two remaining documented bits are compatible options.
    if flags & !COINIT_VALID_FLAGS != 0 || flags & 1 != 0 {
        return Ok(E_INVALIDARG);
    }
    let apartment = flags & COINIT_APARTMENTTHREADED != 0;
    let thread_id = p.current_thread_id;
    if let Some((count, initialized_sta)) = p.com_init.get_mut(&thread_id) {
        if *initialized_sta != apartment {
            return Ok(RPC_E_CHANGED_MODE);
        }
        *count = count.saturating_add(1);
        Ok(S_FALSE)
    } else {
        p.com_init.insert(thread_id, (1, apartment));
        Ok(S_OK)
    }
}

fn co_uninitialize(p: &mut Process) -> R {
    let thread_id = p.current_thread_id;
    let remove = match p.com_init.get_mut(&thread_id) {
        Some((count, _)) if *count <= 1 => true,
        Some((count, _)) => {
            *count -= 1;
            false
        }
        None => false,
    };
    if remove {
        p.com_init.remove(&thread_id);
    }
    Ok(0)
}

fn co_create_instance(p: &mut Process) -> R {
    let (class_id, outer, context, iid, out) =
        (p.arg(0), p.arg(1), p.arg(2) as u32, p.arg(3), p.arg(4));
    if out == 0 || class_id == 0 || iid == 0 {
        return Ok(E_POINTER);
    }
    p.wrptr(out, 0)?;
    if outer != 0 {
        return Ok(CLASS_E_NOAGGREGATION);
    }
    if context & CLSCTX_INPROC_SERVER == 0 {
        return Ok(REGDB_E_CLASSNOTREG);
    }

    if p.read_bytes(class_id, 16)?.as_slice() != CLSID_DIRECTSOUND8 {
        return Ok(REGDB_E_CLASSNOTREG);
    }
    if p.read_bytes(iid, 16)?.as_slice() != IID_IDIRECTSOUND8 {
        return Ok(E_NOINTERFACE);
    }

    let object = com::create(p, "IDirectSound8", com::Kind::DirectSound);
    p.wrptr(out, object)?;
    Ok(S_OK)
}

fn co_get_class_object(p: &mut Process) -> R {
    let out = p.arg(4);
    if out != 0 {
        p.wrptr(out, 0)?;
    }
    Ok(REGDB_E_CLASSNOTREG)
}

fn co_task_mem_alloc(p: &mut Process) -> R {
    Ok(kernel32::alloc_block(p, p.arg(0), false))
}

fn co_task_mem_realloc(p: &mut Process) -> R {
    let (block, requested) = (p.arg(0), p.arg(1));
    if block == 0 {
        return Ok(kernel32::alloc_block(p, requested, false));
    }
    if requested == 0 {
        kernel32::free_block(p, block);
        return Ok(0);
    }

    let Some(old_size) = p.heap.size_of(block) else {
        return Ok(0);
    };
    if old_size == requested {
        return Ok(block);
    }

    let new_block = kernel32::alloc_block(p, requested, false);
    if new_block == 0 {
        return Ok(0);
    }
    let retained = old_size.min(requested) as usize;
    let data = p.read_bytes(block, retained)?;
    p.write_bytes(new_block, &data)?;
    kernel32::free_block(p, block);
    Ok(new_block)
}

fn co_task_mem_free(p: &mut Process) -> R {
    let block = p.arg(0);
    if block != 0 {
        kernel32::free_block(p, block);
    }
    Ok(0)
}

fn co_create_guid(p: &mut Process) -> R {
    let out = p.arg(0);
    if out == 0 {
        return Ok(E_POINTER);
    }

    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&p.next_rand().to_le_bytes());
    bytes[8..].copy_from_slice(&p.next_rand().to_le_bytes());
    // Set the UUID version and variant bits in the in-memory GUID representation.
    bytes[7] = (bytes[7] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    p.write_bytes(out, &bytes)?;
    Ok(S_OK)
}

fn iid_from_string(p: &mut Process) -> R {
    let (string, out) = (p.arg(0), p.arg(1));
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    if string == 0 {
        p.write_bytes(out, &[0; 16])?;
        return Ok(S_OK);
    }

    let string = p.read_wstr(string)?;
    let Some(bytes) = parse_guid(&string) else {
        return Ok(E_INVALIDARG);
    };
    p.write_bytes(out, &bytes)?;
    Ok(S_OK)
}

fn string_from_guid2(p: &mut Process) -> R {
    let (guid, out, capacity) = (p.arg(0), p.arg(1), p.arg(2) as u32 as i32);
    const REQUIRED_CHARS: i32 = 39;
    if capacity < REQUIRED_CHARS {
        return Ok(0);
    }

    let bytes = p.read_bytes(guid, 16)?;
    let string = format_guid(&bytes);
    p.write_wstr(out, &string)?;
    Ok(REQUIRED_CHARS as u64)
}

fn parse_guid(text: &str) -> Option<[u8; 16]> {
    let text = text
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(text);
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 5
        || parts[0].len() != 8
        || parts[1].len() != 4
        || parts[2].len() != 4
        || parts[3].len() != 4
        || parts[4].len() != 12
    {
        return None;
    }

    let data1 = u32::from_str_radix(parts[0], 16).ok()?;
    let data2 = u16::from_str_radix(parts[1], 16).ok()?;
    let data3 = u16::from_str_radix(parts[2], 16).ok()?;
    let tail = format!("{}{}", parts[3], parts[4]);
    let mut bytes = [0u8; 16];
    bytes[..4].copy_from_slice(&data1.to_le_bytes());
    bytes[4..6].copy_from_slice(&data2.to_le_bytes());
    bytes[6..8].copy_from_slice(&data3.to_le_bytes());
    for (i, pair) in tail.as_bytes().chunks_exact(2).enumerate() {
        bytes[8 + i] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

fn format_guid(bytes: &[u8]) -> String {
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
        u16::from_le_bytes(bytes[4..6].try_into().unwrap()),
        u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x10000;
    const STACK: u64 = BASE + 0x800;
    const GUID: u64 = BASE + 0x100;
    const OUT: u64 = BASE + 0x200;
    const WIDE: u64 = BASE + 0x300;
    const TEST_GUID: [u8; 16] = [
        0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p
    }

    fn call(p: &mut Process, name: &str, args: &[u32]) -> u64 {
        for (i, value) in args.iter().enumerate() {
            p.wr32(STACK + 4 + 4 * i as u64, *value).unwrap();
        }
        lookup(name).expect("OLE32 handler is registered")(p).unwrap()
    }

    #[test]
    fn task_allocator_reallocates_preserves_data_and_frees() {
        let mut p = process();
        let first = call(&mut p, "CoTaskMemAlloc", &[24]);
        assert_ne!(first, 0);
        assert!(p.heap.size_of(first).unwrap() >= 24);
        p.write_bytes(first, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();

        let resized = call(&mut p, "CoTaskMemRealloc", &[first as u32, 64]);
        assert_ne!(resized, 0);
        assert_eq!(p.read_bytes(resized, 8).unwrap(), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(p.heap.size_of(resized).unwrap() >= 64);
        assert_eq!(p.heap.size_of(first), None);

        let shrunk = call(&mut p, "CoTaskMemRealloc", &[resized as u32, 8]);
        assert_ne!(shrunk, 0);
        assert_eq!(p.read_bytes(shrunk, 8).unwrap(), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(p.heap.size_of(shrunk).unwrap() >= 8);
        assert_eq!(p.heap.size_of(resized), None);

        assert_eq!(call(&mut p, "CoTaskMemFree", &[shrunk as u32]), 0);
        assert_eq!(p.heap.size_of(shrunk), None);
        assert_eq!(p.heap.bytes_in_use, 0);
        assert_eq!(call(&mut p, "CoTaskMemFree", &[0]), 0);
    }

    #[test]
    fn task_realloc_zero_frees_and_null_realloc_allocates() {
        let mut p = process();
        let zero_sized = call(&mut p, "CoTaskMemAlloc", &[0]);
        assert_ne!(zero_sized, 0);
        assert!(p.heap.size_of(zero_sized).unwrap() >= 1);
        call(&mut p, "CoTaskMemFree", &[zero_sized as u32]);

        let first = call(&mut p, "CoTaskMemAlloc", &[16]);
        assert_ne!(first, 0);
        assert_eq!(call(&mut p, "CoTaskMemRealloc", &[first as u32, 0]), 0);
        assert_eq!(p.heap.size_of(first), None);

        let zero_sized = call(&mut p, "CoTaskMemRealloc", &[0, 0]);
        assert_ne!(zero_sized, 0);
        assert!(p.heap.size_of(zero_sized).unwrap() >= 1);
    }

    #[test]
    fn string_guid_round_trip_uses_ole_guid_layout() {
        let mut p = process();
        p.write_bytes(GUID, &TEST_GUID).unwrap();
        assert_eq!(
            call(&mut p, "StringFromGUID2", &[GUID as u32, WIDE as u32, 39]),
            39
        );
        assert_eq!(
            p.read_wstr(WIDE).unwrap(),
            "{00112233-4455-6677-8899-aabbccddeeff}"
        );
        assert_eq!(p.read_bytes(WIDE + 76, 2).unwrap(), [0, 0]);

        assert_eq!(
            call(&mut p, "IIDFromString", &[WIDE as u32, OUT as u32]),
            S_OK
        );
        assert_eq!(p.read_bytes(OUT, 16).unwrap(), TEST_GUID);
    }

    #[test]
    fn guid_string_small_buffer_is_not_modified_and_bad_iid_is_rejected() {
        let mut p = process();
        p.write_bytes(GUID, &TEST_GUID).unwrap();
        p.write_bytes(WIDE, &[0xaa, 0xbb]).unwrap();
        assert_eq!(
            call(&mut p, "StringFromGUID2", &[GUID as u32, WIDE as u32, 38]),
            0
        );
        assert_eq!(p.read_bytes(WIDE, 2).unwrap(), [0xaa, 0xbb]);

        p.write_wstr(WIDE, "not-a-guid").unwrap();
        p.write_bytes(OUT, &[0xaa; 16]).unwrap();
        assert_eq!(
            call(&mut p, "IIDFromString", &[WIDE as u32, OUT as u32]),
            E_INVALIDARG
        );
        assert_eq!(p.read_bytes(OUT, 16).unwrap(), [0xaa; 16]);
    }

    #[test]
    fn iid_from_null_string_writes_guid_null() {
        let mut p = process();
        p.write_bytes(OUT, &[0xaa; 16]).unwrap();
        assert_eq!(call(&mut p, "IIDFromString", &[0, OUT as u32]), S_OK);
        assert_eq!(p.read_bytes(OUT, 16).unwrap(), [0; 16]);
        assert_eq!(call(&mut p, "IIDFromString", &[0, 0]), E_INVALIDARG);
    }

    #[test]
    fn create_guid_sets_rfc4122_version_and_variant_bits() {
        let mut p = process();
        assert_eq!(call(&mut p, "CoCreateGuid", &[OUT as u32]), S_OK);
        let first = p.read_bytes(OUT, 16).unwrap();
        assert_eq!(first[7] & 0xf0, 0x40);
        assert_eq!(first[8] & 0xc0, 0x80);

        assert_eq!(call(&mut p, "CoCreateGuid", &[OUT as u32]), S_OK);
        assert_ne!(p.read_bytes(OUT, 16).unwrap(), first);
        assert_eq!(call(&mut p, "CoCreateGuid", &[0]), E_POINTER);
    }

    #[test]
    fn com_initialization_counts_nesting_and_rejects_apartment_changes() {
        let mut p = process();
        assert_eq!(call(&mut p, "CoInitialize", &[0]), S_OK);
        assert_eq!(call(&mut p, "CoInitializeEx", &[0, 2 | 4]), S_FALSE);
        assert_eq!(call(&mut p, "CoInitializeEx", &[0, 0]), RPC_E_CHANGED_MODE);

        assert_eq!(call(&mut p, "CoUninitialize", &[]), 0);
        assert_eq!(call(&mut p, "CoInitializeEx", &[0, 2]), S_FALSE);
        assert_eq!(call(&mut p, "CoUninitialize", &[]), 0);
        assert_eq!(call(&mut p, "CoUninitialize", &[]), 0);

        assert_eq!(call(&mut p, "CoInitializeEx", &[0, 0]), S_OK);
        assert_eq!(call(&mut p, "CoInitialize", &[0]), RPC_E_CHANGED_MODE);
    }

    #[test]
    fn com_initialization_rejects_invalid_parameters_without_changing_state() {
        let mut p = process();
        assert_eq!(call(&mut p, "CoInitialize", &[1]), E_INVALIDARG);
        assert_eq!(call(&mut p, "CoInitializeEx", &[1, 0]), E_INVALIDARG);
        assert_eq!(call(&mut p, "CoInitializeEx", &[0, 1]), E_INVALIDARG);
        assert_eq!(call(&mut p, "CoInitializeEx", &[0, 0x10]), E_INVALIDARG);
        assert!(p.com_init.is_empty());

        assert_eq!(call(&mut p, "CoUninitialize", &[]), 0);
        assert!(p.com_init.is_empty());
        assert_eq!(call(&mut p, "CoInitialize", &[0]), S_OK);
    }

    #[test]
    fn com_initialization_state_is_isolated_per_thread() {
        let mut p = process();
        let main_thread = p.current_thread_id;
        assert_eq!(call(&mut p, "CoInitialize", &[0]), S_OK);

        p.current_thread_id = 0x2222;
        assert_eq!(call(&mut p, "CoInitializeEx", &[0, 0]), S_OK);

        p.current_thread_id = main_thread;
        assert_eq!(call(&mut p, "CoInitialize", &[0]), S_FALSE);
        assert_eq!(call(&mut p, "CoInitializeEx", &[0, 0]), RPC_E_CHANGED_MODE);

        p.current_thread_id = 0x2222;
        assert_eq!(call(&mut p, "CoInitializeEx", &[0, 0]), S_FALSE);
        assert_eq!(call(&mut p, "CoInitialize", &[0]), RPC_E_CHANGED_MODE);
    }
}
