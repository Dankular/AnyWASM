//! Host implementations of Windows system libraries ("HLE"), organised by library group.
//!
//! Imports are bound by *name*; DLL names are mapped to a group with [`group_of`] (so
//! `api-ms-win-crt-stdio-l1-1-0.dll`, `ucrtbase.dll` and `msvcrt.dll` all share the `crt` group).

pub mod advapi32;
pub mod com;
pub mod crt;
pub mod d3d11;
pub mod d3d9;
pub mod ddraw;
pub mod directinput8;
pub mod dsound;
pub mod dxgi;
pub mod gdi;
pub mod gpu;
pub mod heap;
pub mod kernel32;
pub mod ntdll;
pub mod ole32;
pub mod printf;
pub mod shell32;
pub mod shlwapi;
pub mod user32;
pub mod vtables;

use crate::process::{HleFn, Process, Stop};
use std::collections::HashMap;

/// Private state of the host libraries.
#[derive(Default)]
pub struct Extra {
    pub cmdline_a: u64,
    pub cmdline_w: u64,
    pub env_w: u64,
    pub iob: u64,
    pub files: HashMap<u64, crt::CFile>,
    pub data_cells: HashMap<String, u64>,
    pub data_next: u64,
    pub sleep_adjust: u64,
    pub ddraw_mode: Option<(u32, u32, u32)>,
    pub argv: u64,
    pub argc: u32,
    pub errno_cell: u64,
    pub rand_state: u32,
    pub strtok_ptr: u64,
    pub locale_cell: u64,
    pub ctype_table: u64,
    pub exit_hooks: Vec<u64>,
    pub onexit_table: Vec<u64>,
    pub cpp_exception_cells: HashMap<u32, (u64, u64)>,
    pub tmp_a: u64,
    pub win: user32::WinState,
    pub gpu: gpu::GpuStream,
    pub com: com::ComState,
}

/// Map a canonical (lower-case, no ".dll") library name to a host library group.
pub fn group_of(name: &str) -> Option<&'static str> {
    if name == "kernel32"
        || name == "kernelbase"
        || name.starts_with("api-ms-win-core-")
        || name.starts_with("api-ms-win-eventing-")
        || name == "api-ms-win-security-base-l1-1-0"
    {
        return Some("kernel32");
    }
    if name == "ntdll" {
        return Some("ntdll");
    }
    if name.starts_with("msvcp") {
        return Some("msvcp");
    }
    if name == "msvcrt"
        || name == "ucrtbase"
        || name.starts_with("api-ms-win-crt-")
        || name.starts_with("vcruntime")
        || name.starts_with("msvcr")
        || name == "ucrtbased"
    {
        return Some("crt");
    }
    match name {
        "user32" => Some("user32"),
        "shlwapi" => Some("shlwapi"),
        "winmm" => Some("winmm"),
        "d3d11" => Some("d3d11"),
        // GTA San Andreas probes for d3d8.dll as a DirectX runtime-presence gate before it
        // continues to its D3D9 startup path; this maps the module load only, not D3D8 exports.
        "d3d8" => Some("d3d8"),
        "dsound" => Some("dsound"),
        // GTA loads this DirectPlay helper as part of its DirectX runtime check. No exports are
        // supplied until a guest call demonstrates that the game needs one.
        "dpnhpast" => Some("dpnhpast"),
        "d3d9" => Some("d3d9"),
        "dinput8" => Some("dinput8"),
        "ddraw" => Some("ddraw"),
        "dxgi" => Some("dxgi"),
        "gdi32" => Some("gdi32"),
        "advapi32" => Some("advapi32"),
        "shell32" => Some("shell32"),
        "ole32" => Some("ole32"),
        _ => None,
    }
}

/// Find the host implementation of `group!name`.
pub fn lookup(group: &str, name: &str) -> Option<HleFn> {
    match group {
        // kernel32 re-exports a few ntdll functions and vice versa; try both.
        "kernel32" => kernel32::lookup(name).or_else(|| ntdll::lookup(name)),
        "ntdll" => ntdll::lookup(name).or_else(|| crt::lookup(name)),
        "msvcp" => crt::lookup(name),
        "ole32" => ole32::lookup(name),
        "crt" => crt::lookup(name),
        "advapi32" => advapi32::lookup(name),
        "user32" => user32::lookup(name),
        "gdi32" => gdi::lookup(name),
        "shell32" => shell32::lookup(name),
        "shlwapi" => shlwapi::lookup(name),
        "winmm" => user32::lookup_winmm(name),
        "d3d11" => d3d11::lookup(name),
        "d3d9" => d3d9::lookup(name),
        "dsound" => dsound::lookup(name),
        "ddraw" => ddraw::lookup(name),
        "dinput8" => directinput8::lookup(name),
        "dxgi" => dxgi::lookup(name),
        "com" => com::lookup(name),
        _ => None,
    }
}

/// Imports that are *data* (a variable the program reads), not code.
pub fn data_symbol(group: &str, name: &str) -> Option<u64> {
    if group == "msvcp" {
        return match name {
            crt::MSVC_STD_COUT_SYMBOL | crt::MSVC_STD_CERR_SYMBOL => Some(0xe0),
            _ => None,
        };
    }
    if group != "crt" {
        return None;
    }
    match name {
        "_fmode" | "_commode" | "__argc" | "_osver" | "_winver" | "_winmajor" | "_winminor"
        | "__mb_cur_max" => Some(8),
        "__initenv" | "__winitenv" | "_environ" | "_wenviron" | "__argv" | "__wargv"
        | "_pgmptr" | "_wpgmptr" | "_acmdln" | "_wcmdln" | "_iob" => Some(8),
        _ => None,
    }
}

/// Allocate (once) a zeroed guest data cell for a data import and return its address.
pub fn data_cell(p: &mut Process, key: &str, size: u64) -> u64 {
    if let Some(&a) = p.extra.data_cells.get(key) {
        return a;
    }
    if p.extra.data_next == 0 {
        p.extra.data_next = p.alloc_virtual(0x10000, anywasm_cpu::mem::PROT_RW);
    }
    let a = p.extra.data_next;
    p.extra.data_next += (size + 15) & !15;
    p.extra.data_cells.insert(key.to_string(), a);
    a
}

#[cfg(test)]
mod advapi32_hle_tests {
    use super::lookup;

    #[test]
    fn systemfunction036_resolves_from_advapi32() {
        assert!(lookup("advapi32", "SystemFunction036").is_some());
    }
}

#[cfg(test)]
mod crt_locale_lock_tests {
    use super::lookup;
    use crate::{process::Process, vfs::Vfs};

    #[test]
    fn locale_lock_imports_are_reentrant_and_release_the_lock() {
        let lock = lookup("crt", "_lock_locales").expect("UCRT locale lock should resolve");
        let unlock = lookup("crt", "_unlock_locales").expect("UCRT locale unlock should resolve");
        let mut process = Process::new(Vfs::new());
        process.current_thread_id = 0x1234;

        lock(&mut process).unwrap();
        lock(&mut process).unwrap();
        let state = process.critical_sections.values().next().unwrap();
        assert_eq!(state.owner, Some(0x1234));
        assert_eq!(state.recursion, 2);

        unlock(&mut process).unwrap();
        assert_eq!(
            process.critical_sections.values().next().unwrap().recursion,
            1
        );
        unlock(&mut process).unwrap();
        let state = process.critical_sections.values().next().unwrap();
        assert_eq!(state.owner, None);
        assert_eq!(state.recursion, 0);
    }
}

#[cfg(test)]
mod crt_stream_pointer_tests {
    use super::lookup;
    use crate::{process::Process, vfs::Vfs};
    use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
    use anywasm_cpu::mem::PROT_RW;

    #[test]
    fn get_stream_buffer_pointers_returns_addresses_of_file_fields() {
        let get_pointers = lookup("crt", "_get_stream_buffer_pointers")
            .expect("UCRT stream buffer helper should resolve");
        let mut process = Process::new(Vfs::new());
        let stream = process.alloc_virtual(0x30, PROT_RW);
        let outputs = process.alloc_virtual(0x1000, PROT_RW);
        process.cpu.gpr[RCX] = stream;
        process.cpu.gpr[RDX] = outputs;
        process.cpu.gpr[R8] = outputs + 8;
        process.cpu.gpr[R9] = outputs + 16;

        assert_eq!(get_pointers(&mut process).unwrap(), 0);
        assert_eq!(process.rd64(outputs).unwrap(), stream + 8);
        assert_eq!(process.rd64(outputs + 8).unwrap(), stream);
        assert_eq!(process.rd64(outputs + 16).unwrap(), stream + 16);
    }
}

#[cfg(test)]
mod crt_setlocale_tests {
    use super::lookup;
    use crate::{process::Process, vfs::Vfs};
    use anywasm_cpu::cpu::{RCX, RDX};

    #[test]
    fn default_locale_query_returns_the_c_locale_name() {
        let setlocale = lookup("crt", "setlocale").expect("UCRT setlocale should resolve");
        let mut process = Process::new(Vfs::new());
        process.cpu.gpr[RCX] = 0; // LC_ALL
        process.cpu.gpr[RDX] = 0; // query current locale

        let name = setlocale(&mut process).unwrap();

        assert_ne!(name, 0);
        assert_eq!(process.mem.read_cstr(name, 4).unwrap(), b"C");
    }
}

pub type R = Result<u64, Stop>;

pub fn ok(v: u64) -> R {
    Ok(v)
}
