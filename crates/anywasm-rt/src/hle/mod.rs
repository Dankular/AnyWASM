//! Host implementations of Windows system libraries ("HLE"), organised by library group.
//!
//! Imports are bound by *name*; DLL names are mapped to a group with [`group_of`] (so
//! `api-ms-win-crt-stdio-l1-1-0.dll`, `ucrtbase.dll` and `msvcrt.dll` all share the `crt` group).

pub mod advapi32;
pub mod com;
pub mod crt;
pub mod d3d11;
pub mod d3d9;
pub mod d3dx9_31;
pub mod dbghelp;
pub mod ddraw;
pub mod directinput8;
pub mod dsound;
pub mod dxgi;
pub mod eax;
pub mod gdi;
pub mod gpu;
pub mod heap;
pub mod imm32;
pub mod iphlpapi;
pub mod kernel32;
pub mod netapi32;
pub mod ntdll;
pub mod ole32;
pub mod oleaut32;
pub mod printf;
pub mod psapi;
pub mod setupapi;
pub mod shell32;
pub mod shlwapi;
pub mod user32;
pub mod usp10;
pub mod version;
pub mod vorbisfile;
pub mod vtables;
pub mod wininet;
pub mod ws2_32;

use crate::process::{HleFn, Process, Stop};
use std::collections::HashMap;

/// Private state of the host libraries.
#[derive(Default)]
pub struct Extra {
    pub advapi32: advapi32::AdvapiState,
    pub imm32: imm32::ImmState,
    pub ws2_32: ws2_32::Ws2State,
    pub wininet: wininet::WinInetState,
    pub vorbisfile: vorbisfile::VorbisState,
    pub version: version::VersionState,
    /// Process-local byte-range locks made through kernel32 LockFile.
    pub file_range_locks: Vec<kernel32::FileRangeLock>,
    /// Active file-backed views created through kernel32 MapViewOfFile.
    pub file_mapping_views: HashMap<u64, kernel32::FileMappingView>,
    /// Desired access captured by CreateFile for validating file-lock calls.
    pub file_access: HashMap<u64, u32>,
    pub cmdline_a: u64,
    pub cmdline_w: u64,
    pub env_w: u64,
    pub iob: u64,
    pub files: HashMap<u64, crt::CFile>,
    pub data_cells: HashMap<String, u64>,
    pub data_next: u64,
    pub sleep_adjust: u64,
    /// Per guest-thread execution-state flags; only the documented ES_CONTINUOUS reset is modeled.
    pub thread_execution_states: HashMap<u32, u32>,
    pub ddraw_mode: Option<(u32, u32, u32)>,
    pub argv: u64,
    pub wargv: u64,
    pub argc: u32,
    pub env_a: u64,
    pub initial_env_a: u64,
    pub initial_env_w: u64,
    pub program_a: u64,
    pub program_w: u64,
    pub errno_cell: u64,
    pub rand_state: u32,
    pub strtok_ptr: u64,
    /// `_localtime64`'s CRT `tm` result buffer, indexed by guest thread ID.
    pub localtime_buffers: HashMap<u32, u64>,
    pub locale_cell: u64,
    pub ctype_table: u64,
    pub exit_hooks: Vec<u64>,
    pub onexit_table: Vec<u64>,
    pub cpp_exception_cells: HashMap<u32, (u64, u64)>,
    pub purecall_handler: u64,
    pub invalid_parameter_handler: u64,
    pub abort_behavior_flags: Option<u32>,
    pub tmp_a: u64,
    pub win: user32::WinState,
    pub gpu: gpu::GpuStream,
    pub com: com::ComState,
    pub dbghelp: dbghelp::DbgHelpState,
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
        "dsound" => Some("dsound"),
        "eax" => Some("eax"),
        "vorbisfile" => Some("vorbisfile"),
        "d3dx9_31" => Some("d3dx9_31"),
        "d3d9" => Some("d3d9"),
        "dinput8" => Some("dinput8"),
        "ddraw" => Some("ddraw"),
        "dxgi" => Some("dxgi"),
        "gdi32" => Some("gdi32"),
        "advapi32" => Some("advapi32"),
        "dbghelp" => Some("dbghelp"),
        "shell32" => Some("shell32"),
        "setupapi" => Some("setupapi"),
        "ole32" => Some("ole32"),
        "oleaut32" => Some("oleaut32"),
        "version" => Some("version"),
        "imm32" => Some("imm32"),
        "ws2_32" => Some("ws2_32"),
        "psapi" => Some("psapi"),
        "iphlpapi" => Some("iphlpapi"),
        "wininet" => Some("wininet"),
        "usp10" => Some("usp10"),
        "netapi32" => Some("netapi32"),
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
        "oleaut32" => oleaut32::lookup(name),
        "version" => version::lookup(name),
        "imm32" => imm32::lookup(name),
        "ws2_32" => ws2_32::lookup(name),
        "psapi" => psapi::lookup(name),
        "iphlpapi" => iphlpapi::lookup(name),
        "wininet" => wininet::lookup(name),
        "usp10" => usp10::lookup(name),
        "netapi32" => netapi32::lookup(name),
        "crt" => crt::lookup(name),
        "advapi32" => advapi32::lookup(name),
        "dbghelp" => dbghelp::lookup(name),
        "user32" => user32::lookup(name),
        "gdi32" => gdi::lookup(name),
        "shell32" => shell32::lookup(name),
        "setupapi" => setupapi::lookup(name),
        "shlwapi" => shlwapi::lookup(name),
        "winmm" => user32::lookup_winmm(name),
        "d3d11" => d3d11::lookup(name),
        "d3d9" => d3d9::lookup(name),
        "dsound" => dsound::lookup(name),
        "eax" => eax::lookup(name),
        "vorbisfile" => vorbisfile::lookup(name),
        "d3dx9_31" => d3dx9_31::lookup(name),
        "ddraw" => ddraw::lookup(name),
        "dinput8" => directinput8::lookup(name),
        "dxgi" => dxgi::lookup(name),
        "com" => com::lookup(name),
        _ => None,
    }
}

/// Whether an import has a usable runtime implementation.
///
/// A small number of exports have diagnostic-only handlers so a call can stop with a more
/// specific `Stop::Unimplemented` reason than the generic unresolved-import path. Those
/// handlers are useful at runtime but must not make `analyze` claim that the import is covered.
pub fn import_is_implemented(group: &str, name: &str) -> bool {
    let diagnostic_only = match group {
        "kernel32" => matches!(
            name,
            "CreatePipe"
                | "CreateNamedPipeW"
                | "ConnectNamedPipe"
                | "DisconnectNamedPipe"
                | "CreateProcessW"
                | "DeviceIoControl"
        ),
        "crt" => matches!(name, "_beginthread" | "strspn"),
        _ => false,
    };
    !diagnostic_only && lookup(group, name).is_some()
}

#[cfg(test)]
mod import_coverage_tests {
    use super::*;

    #[test]
    fn diagnostic_only_import_handlers_are_not_reported_as_implemented() {
        for (group, name) in [
            ("kernel32", "CreatePipe"),
            ("crt", "_beginthread"),
            ("crt", "strspn"),
        ] {
            assert!(
                lookup(group, name).is_some(),
                "{group}!{name} diagnostic handler"
            );
            assert!(
                !import_is_implemented(group, name),
                "{group}!{name} must remain visible as an uncovered import"
            );
        }
        assert!(
            import_is_implemented("kernel32", "QueueUserAPC"),
            "QueueUserAPC queues callbacks and dispatches them at alertable waits"
        );
        assert!(import_is_implemented("user32", "CreateWindowExA"));
        assert!(import_is_implemented("kernel32", "CreateFileW"));
        assert!(import_is_implemented("kernel32", "GetWriteWatch"));
        assert!(import_is_implemented("crt", "strcspn"));
        assert!(import_is_implemented("crt", "strpbrk"));
        assert!(import_is_implemented("user32", "MessageBoxA"));
        assert!(import_is_implemented("user32", "MessageBoxW"));
        assert!(import_is_implemented("user32", "DialogBoxParamA"));
        assert!(import_is_implemented("user32", "EndDialog"));
        assert!(import_is_implemented("user32", "CreateIconIndirect"));
        assert!(import_is_implemented("user32", "CreateIconFromResource"));
        assert!(import_is_implemented("user32", "WaitForInputIdle"));
        assert!(import_is_implemented("user32", "FillRect"));
        assert!(import_is_implemented("kernel32", "OpenEventW"));
        assert!(import_is_implemented("kernel32", "OpenMutexW"));
        assert!(import_is_implemented("kernel32", "OpenSemaphoreW"));
    }
}

/// Imports that are *data* (a variable the program reads), not code.
pub fn data_symbol(group: &str, name: &str) -> Option<u64> {
    if group == "msvcp" {
        return match name {
            crt::MSVC_STD_COUT_SYMBOL | crt::MSVC_STD_CERR_SYMBOL => Some(0xe0),
            crt::MSVC_STD_CIN_SYMBOL => Some(0x80),
            crt::MSVC_WCHAR_CTYPE_ID_SYMBOL => Some(8),
            crt::MSVC_LOCALE_ID_COUNT_SYMBOL => Some(4),
            _ => None,
        };
    }
    if group != "crt" {
        return None;
    }
    match name {
        "_HUGE" => Some(8),
        "_adjust_fdiv" => Some(4),
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
mod import_coverage_batch_tests {
    use super::crt::{MSVC_STD_CIN_SYMBOL, MSVC_STD_COUT_SYMBOL};
    use super::{data_symbol, group_of, lookup};

    #[test]
    fn psapi_module_information_and_vc80_data_imports_are_wired() {
        assert_eq!(group_of("psapi"), Some("psapi"));
        assert!(lookup("psapi", "GetModuleInformation").is_some());
        assert_eq!(group_of("iphlpapi"), Some("iphlpapi"));
        assert!(lookup("iphlpapi", "GetAdaptersInfo").is_some());
        assert!(lookup("iphlpapi", "GetAdaptersAddresses").is_some());
        assert_eq!(group_of("oleaut32"), Some("oleaut32"));
        assert!(lookup("oleaut32", "#4").is_some());
        assert_eq!(group_of("wininet"), Some("wininet"));
        assert!(lookup("wininet", "InternetOpenA").is_some());
        assert_eq!(group_of("d3dx9_31"), Some("d3dx9_31"));
        assert!(lookup("d3dx9_31", "D3DXGetShaderSize").is_some());
        assert_eq!(group_of("setupapi"), Some("setupapi"));
        assert!(lookup("setupapi", "SetupDiGetClassDevsA").is_some());
        assert!(lookup("shell32", "ShellExecuteExW").is_some());
        assert_eq!(group_of("usp10"), Some("usp10"));
        assert!(lookup("usp10", "ScriptItemize").is_some());
        assert_eq!(group_of("netapi32"), Some("netapi32"));
        assert!(lookup("netapi32", "Netbios").is_some());
        assert_eq!(group_of("eax"), Some("eax"));
        assert!(lookup("eax", "#6").is_some());
        assert_eq!(group_of("vorbisfile"), Some("vorbisfile"));
        assert!(lookup("vorbisfile", "ov_open_callbacks").is_some());
        assert_eq!(group_of("d3d8"), None, "D3D8 is not translated");
        assert_eq!(group_of("dpnhpast"), None, "dpnhpast is not translated");
        assert_eq!(data_symbol("msvcp", MSVC_STD_CIN_SYMBOL), Some(0x80));
        assert_eq!(data_symbol("msvcp", MSVC_STD_COUT_SYMBOL), Some(0xe0));
        assert_eq!(data_symbol("crt", "_HUGE"), Some(8));
        assert_eq!(data_symbol("crt", "_adjust_fdiv"), Some(4));
    }
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
