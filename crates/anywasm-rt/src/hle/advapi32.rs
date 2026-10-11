use crate::process::{HleFn, Process};
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
        "SystemFunction036" => Some(system_function_036),
        _ => None,
    }
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
    use super::system_function_036_with;
    use crate::process::Process;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{RCX, RDX};
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
}
