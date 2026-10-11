//! Host implementations of Windows system libraries ("HLE"), organised by library group.
//!
//! Imports are bound by *name*; DLL names are mapped to a group with [`group_of`] (so
//! `api-ms-win-crt-stdio-l1-1-0.dll`, `ucrtbase.dll` and `msvcrt.dll` all share the `crt` group).

pub mod com;
pub mod crt;
pub mod d3d11;
pub mod dxgi;
pub mod gpu;
pub mod heap;
pub mod kernel32;
pub mod ntdll;
pub mod printf;
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
    pub argv: u64,
    pub argc: u32,
    pub errno_cell: u64,
    pub rand_state: u32,
    pub strtok_ptr: u64,
    pub locale_cell: u64,
    pub ctype_table: u64,
    pub exit_hooks: Vec<u64>,
    pub onexit_table: Vec<u64>,
    pub tmp_a: u64,
    pub win: user32::WinState,
    pub gpu: gpu::GpuStream,
    pub com: com::ComState,
}

/// Map a canonical (lower-case, no ".dll") library name to a host library group.
pub fn group_of(name: &str) -> Option<&'static str> {
    if name == "kernel32" || name == "kernelbase" || name.starts_with("api-ms-win-core-") || name.starts_with("api-ms-win-eventing-") || name == "api-ms-win-security-base-l1-1-0" {
        return Some("kernel32");
    }
    if name == "ntdll" {
        return Some("ntdll");
    }
    if name == "msvcrt" || name == "ucrtbase" || name.starts_with("api-ms-win-crt-") || name.starts_with("vcruntime") || name.starts_with("msvcr") || name == "ucrtbased" {
        return Some("crt");
    }
    match name {
        "user32" => Some("user32"),
        "winmm" => Some("winmm"),
        "d3d11" => Some("d3d11"),
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
        "crt" => crt::lookup(name),
        "user32" => user32::lookup(name),
        "winmm" => user32::lookup_winmm(name),
        "d3d11" => d3d11::lookup(name),
        "dxgi" => dxgi::lookup(name),
        "com" => com::lookup(name),
        _ => None,
    }
}

/// Imports that are *data* (a variable the program reads), not code.
pub fn data_symbol(group: &str, name: &str) -> Option<u64> {
    if group != "crt" {
        return None;
    }
    match name {
        "_fmode" | "_commode" | "__argc" | "_osver" | "_winver" | "_winmajor" | "_winminor" | "__mb_cur_max" => Some(8),
        "__initenv" | "__winitenv" | "_environ" | "_wenviron" | "__argv" | "__wargv" | "_pgmptr" | "_wpgmptr" | "_acmdln" | "_wcmdln" | "_iob" => Some(8),
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

pub type R = Result<u64, Stop>;

pub fn ok(v: u64) -> R {
    Ok(v)
}
