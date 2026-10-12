use crate::process::{Handle, HleFn, Process};
use crate::Stop;

#[cfg(windows)]
#[link(name = "bcrypt")]
extern "system" {
    fn BCryptGenRandom(
        algorithm: *mut core::ffi::c_void,
        buffer: *mut u8,
        length: u32,
        flags: u32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "anywasm")]
extern "C" {
    #[link_name = "random_fill"]
    fn anywasm_random_fill(buffer: *mut u8, length: u32) -> u32;
}

pub fn lookup(name: &str) -> Option<HleFn> {
    match name {
        "RegOpenKeyExA" => Some(reg_open_key_ex_a),
        "RegCreateKeyExA" => Some(reg_create_key_ex_a),
        "RegSetValueExA" => Some(reg_set_value_ex_a),
        "RegQueryValueExA" => Some(reg_query_value_ex_a),
        "RegCloseKey" => Some(reg_close_key),
        "SystemFunction036" => Some(system_function_036),
        _ => None,
    }
}

const PREDEFINED_ROOTS: [u32; 5] = [
    0x8000_0000, // HKEY_CLASSES_ROOT
    0x8000_0001, // HKEY_CURRENT_USER
    0x8000_0002, // HKEY_LOCAL_MACHINE
    0x8000_0003, // HKEY_USERS
    0x8000_0005, // HKEY_CURRENT_CONFIG
];

fn is_predefined_root(key: u64) -> bool {
    PREDEFINED_ROOTS.contains(&(key as u32))
}

fn is_registry_handle(process: &Process, key: u64) -> bool {
    is_predefined_root(key) || matches!(process.handles.get(&key), Some(Handle::Other))
}

/// Opens the predefined registry roots supported by the runtime.
///
/// AnyWASM currently has no registry data store, so non-empty subkeys are reported missing rather
/// than synthesized. This is enough for callers that probe optional configuration before using
/// their defaults, while keeping the absence visible to the guest.
fn reg_open_key_ex_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_FILE_NOT_FOUND: u64 = 2;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    const REG_OPTION_OPEN_LINK: u64 = 8;
    let root = process.arg(0);
    let subkey_ptr = process.arg(1);
    let options = process.arg(2) as u32 as u64;
    let access = process.arg(3) as u32;
    let result_ptr = process.arg(4);
    let subkey = if subkey_ptr == 0 {
        String::new()
    } else {
        process.read_cstr(subkey_ptr)?
    };

    if process.trace_imports {
        process.stderr.extend_from_slice(
            format!(
                "[registry] RegOpenKeyExA root={root:#x} subkey={subkey:?} options={options:#x} access={access:#x}\n"
            )
            .as_bytes(),
        );
    }

    if !is_registry_handle(process, root) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if result_ptr == 0 || (options != 0 && options != REG_OPTION_OPEN_LINK) {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    if subkey.is_empty() {
        process.wrptr(result_ptr, root)?;
        return Ok(ERROR_SUCCESS);
    }
    if !is_predefined_root(root) {
        let child = process.new_handle(Handle::Other);
        process.wrptr(result_ptr, child)?;
        return Ok(ERROR_SUCCESS);
    }
    Ok(ERROR_FILE_NOT_FOUND)
}

fn reg_create_key_ex_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    const REG_OPTION_NON_VOLATILE: u64 = 0;
    const REG_OPTION_VOLATILE: u64 = 1;
    const REG_CREATED_NEW_KEY: u32 = 1;

    let (root, subkey_ptr, reserved, class_ptr, options, result_ptr, disposition_ptr) = (
        process.arg(0),
        process.arg(1),
        process.arg(2),
        process.arg(3),
        process.arg(4),
        process.arg(7),
        process.arg(8),
    );
    if !is_registry_handle(process, root) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if reserved != 0
        || !matches!(options, REG_OPTION_NON_VOLATILE | REG_OPTION_VOLATILE)
        || result_ptr == 0
    {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    let subkey = if subkey_ptr == 0 {
        String::new()
    } else {
        process.read_cstr(subkey_ptr)?
    };
    if process.trace_imports {
        process.stderr.extend_from_slice(
            format!("[registry] RegCreateKeyExA root={root:#x} subkey={subkey:?} options={options:#x}\n")
                .as_bytes(),
        );
    }
    // Registry state is intentionally volatile for now. This supports applications that create
    // optional device/configuration keys while allowing reads to fall back to their defaults.
    let key = process.new_handle(Handle::Other);
    process.wrptr(result_ptr, key)?;
    if disposition_ptr != 0 {
        process.wr32(disposition_ptr, REG_CREATED_NEW_KEY)?;
    }
    let _ = class_ptr;
    Ok(ERROR_SUCCESS)
}

fn reg_set_value_ex_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    let (key, reserved) = (process.arg(0), process.arg(2));
    if !is_registry_handle(process, key) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if reserved != 0 {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    Ok(ERROR_SUCCESS)
}

fn reg_query_value_ex_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_FILE_NOT_FOUND: u64 = 2;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    let (key, reserved, type_ptr, size_ptr) = (
        process.arg(0),
        process.arg(2),
        process.arg(3),
        process.arg(5),
    );
    if !is_registry_handle(process, key) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if reserved != 0 {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    if type_ptr != 0 {
        process.wr32(type_ptr, 0)?;
    }
    if size_ptr != 0 {
        process.wr32(size_ptr, 0)?;
    }
    Ok(ERROR_FILE_NOT_FOUND)
}

fn reg_close_key(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_INVALID_HANDLE: u64 = 6;
    let key = process.arg(0);
    if is_predefined_root(key) {
        return Ok(ERROR_SUCCESS);
    }
    Ok(if process.handles.remove(&key).is_some() {
        ERROR_SUCCESS
    } else {
        ERROR_INVALID_HANDLE
    })
}

fn system_function_036(process: &mut Process) -> Result<u64, Stop> {
    system_function_036_with(process, fill_secure_random)
}

fn system_function_036_with(
    process: &mut Process,
    fill_random: impl FnOnce(&mut [u8]) -> bool,
) -> Result<u64, Stop> {
    let destination = process.arg(0);
    let length = process.arg(1) as u32 as usize;
    if length == 0 {
        return Ok(1);
    }

    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(length).is_err() {
        return Ok(0);
    }
    bytes.resize(length, 0);
    if !fill_random(&mut bytes) {
        return Ok(0);
    }

    process.write_bytes(destination, &bytes)?;
    Ok(1)
}

#[cfg(windows)]
fn fill_secure_random(bytes: &mut [u8]) -> bool {
    // BCRYPT_USE_SYSTEM_PREFERRED_RNG
    unsafe {
        BCryptGenRandom(
            core::ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            2,
        ) == 0
    }
}

#[cfg(all(not(windows), not(target_arch = "wasm32")))]
fn fill_secure_random(bytes: &mut [u8]) -> bool {
    use std::io::Read;

    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(bytes))
        .is_ok()
}

#[cfg(target_arch = "wasm32")]
fn fill_secure_random(bytes: &mut [u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    unsafe { anywasm_random_fill(bytes.as_mut_ptr(), bytes.len() as u32) == 1 }
}

#[cfg(test)]
mod tests {
    use super::{lookup, system_function_036_with};
    use crate::process::Process;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x10000;

    fn process_with_buffer(initial: u8) -> (Process, u64) {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
        let buffer = BASE + 0x100;
        process.write_bytes(buffer, &[initial; 4]).unwrap();
        process.cpu.gpr[RCX] = buffer;
        process.cpu.gpr[RDX] = 4;
        (process, buffer)
    }

    #[test]
    fn zero_length_request_succeeds_without_requesting_entropy() {
        let mut process = Process::new(Vfs::new());
        process.cpu.gpr[RCX] = 0;
        process.cpu.gpr[RDX] = 0;

        assert_eq!(
            system_function_036_with(&mut process, |_: &mut [u8]| {
                panic!("zero-length request used entropy")
            }),
            Ok(1)
        );
    }

    #[test]
    fn entropy_failure_returns_false_without_changing_guest_buffer() {
        let (mut process, buffer) = process_with_buffer(0xaa);

        assert_eq!(
            system_function_036_with(&mut process, |bytes: &mut [u8]| {
                bytes.fill(0x55);
                false
            }),
            Ok(0)
        );
        assert_eq!(process.read_bytes(buffer, 4).unwrap(), [0xaa; 4]);
    }

    #[test]
    fn successful_entropy_is_copied_to_guest_buffer() {
        let (mut process, buffer) = process_with_buffer(0xaa);

        assert_eq!(
            system_function_036_with(&mut process, |bytes: &mut [u8]| {
                bytes.fill(0x3c);
                true
            }),
            Ok(1)
        );
        assert_eq!(process.read_bytes(buffer, 4).unwrap(), [0x3c; 4]);
    }

    fn registry_process(root: u64, subkey: Option<&str>) -> (Process, u64) {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
        let stack = BASE + 0x800;
        let result = BASE + 0x200;
        process
            .write_bytes(result, &0xfeed_face_cafe_beefu64.to_le_bytes())
            .unwrap();
        process.cpu.gpr[RCX] = root;
        process.cpu.gpr[RDX] = match subkey {
            Some(subkey) => {
                let address = BASE + 0x100;
                process.write_cstr(address, subkey).unwrap();
                address
            }
            None => 0,
        };
        process.cpu.gpr[R8] = 0;
        process.cpu.gpr[R9] = 0x20019;
        process.cpu.gpr[RSP] = stack;
        process.wr64(stack + 40, result).unwrap();
        (process, result)
    }

    #[test]
    fn reg_open_key_ex_a_returns_predefined_root_for_empty_subkey() {
        const HKEY_CURRENT_USER: u64 = 0xffff_ffff_8000_0001;
        let (mut process, result) = registry_process(HKEY_CURRENT_USER, None);
        let open = lookup("RegOpenKeyExA").expect("RegOpenKeyExA should be implemented");

        assert_eq!(open(&mut process), Ok(0));
        assert_eq!(process.rd64(result), Ok(HKEY_CURRENT_USER));
    }

    #[test]
    fn reg_open_key_ex_a_reports_missing_subkey_without_changing_output() {
        const HKEY_CURRENT_USER: u64 = 0xffff_ffff_8000_0001;
        let (mut process, result) = registry_process(HKEY_CURRENT_USER, Some("Software\\AnyWASM"));
        let open = lookup("RegOpenKeyExA").expect("RegOpenKeyExA should be implemented");

        assert_eq!(open(&mut process), Ok(2)); // ERROR_FILE_NOT_FOUND
        assert_eq!(process.rd64(result), Ok(0xfeed_face_cafe_beef));
    }

    #[test]
    fn reg_open_key_ex_a_rejects_unknown_root_without_changing_output() {
        let (mut process, result) = registry_process(0x1234, None);
        let open = lookup("RegOpenKeyExA").expect("RegOpenKeyExA should be implemented");

        assert_eq!(open(&mut process), Ok(6)); // ERROR_INVALID_HANDLE
        assert_eq!(process.rd64(result), Ok(0xfeed_face_cafe_beef));
    }
}
