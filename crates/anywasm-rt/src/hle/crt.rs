//! C runtime (msvcrt / ucrtbase / vcruntime) host implementation.

use super::printf::{self, Args, SlotArgs, VaList};
use super::*;
use crate::process::*;
use anywasm_cpu::cpu::*;

pub enum FileKind {
    Stdin,
    Stdout,
    Stderr,
    Vfs {
        path: String,
        pos: usize,
        read: bool,
        write: bool,
    },
}

pub struct CFile {
    pub kind: FileKind,
    pub eof: bool,
    pub err: bool,
    pub ungot: Option<u8>,
}

const FILE_SIZE: u64 = 0x30;
pub const MSVC_STD_COUT_SYMBOL: &str = "?cout@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A";
pub const MSVC_STD_CERR_SYMBOL: &str = "?cerr@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A";
pub const MSVC_STD_CIN_SYMBOL: &str = "?cin@std@@3V?$basic_istream@DU?$char_traits@D@std@@@1@A";
pub const MSVC_WCHAR_CTYPE_ID_SYMBOL: &str = "?id@?$ctype@_W@std@@2V0locale@2@A";
pub const MSVC_LOCALE_ID_COUNT_SYMBOL: &str = "?_Id_cnt@id@locale@std@@0HA";
const MSVC_STD_COUT_OBJECT: &str = "msvcp:std::cout:object";
const MSVC_STD_CERR_OBJECT: &str = "msvcp:std::cerr:object";
const MSVC_STD_COUT_BUFFER: &str = "msvcp:std::cout:streambuf";
const MSVC_STD_CERR_BUFFER: &str = "msvcp:std::cerr:streambuf";
const MSVC_STD_COUT_OBJECT_X86: &str = "msvcp:std::cout:object:x86";
const MSVC_STD_CERR_OBJECT_X86: &str = "msvcp:std::cerr:object:x86";
const MSVC_STD_COUT_BUFFER_X86: &str = "msvcp:std::cout:streambuf:x86";
const MSVC_STD_CERR_BUFFER_X86: &str = "msvcp:std::cerr:streambuf:x86";
const MSVC_STD_CIN_OBJECT_X86: &str = "msvcp:std::cin:object:x86";
const MSVC_STD_CIN_BUFFER_X86: &str = "msvcp:std::cin:streambuf:x86";
const MSVC_STD_OSTREAM_VTABLE: &str = "msvcp:std::ostream:vtable";
const MSVC_STD_STREAMBUF_VTABLE: &str = "msvcp:std::streambuf:vtable";
const MSVC_STD_OSTREAM_VBTABLE: &str = "msvcp:std::ostream:vbtable";
const MSVC_STD_LOCALE: &str = "msvcp:std::locale";
const ONEXIT_INITIAL_CAPACITY: u64 = 32;
const ONEXIT_MAX_ENTRIES: u64 = 1 << 20;

/// Data imports (variables): returns the guest address of the cell and initialises it.
pub fn data_import(p: &mut Process, name: &str, size: u64) -> Result<u64, Stop> {
    if matches!(
        name,
        MSVC_STD_COUT_SYMBOL | MSVC_STD_CERR_SYMBOL | MSVC_STD_CIN_SYMBOL
    ) {
        return msvc_standard_stream_import(p, name);
    }

    let a = data_cell(p, name, size);
    match name {
        "__argc" => {
            let argc = p.extra.argc.max(1) as u64;
            p.wr32(a, argc as u32)?;
        }
        "__argv" => {
            ensure_args(p)?;
            p.wrptr(a, p.extra.argv)?;
        }
        "__wargv" => {
            ensure_wide_args(p)?;
            p.wrptr(a, p.extra.wargv)?;
        }
        "_acmdln" => {
            let command_line = kernel32::command_line_a(p)?;
            p.wrptr(a, command_line)?;
        }
        "_wcmdln" => {
            let command_line = kernel32::command_line_w(p)?;
            p.wrptr(a, command_line)?;
        }
        "_pgmptr" => {
            let program = ensure_program_name(p, false)?;
            p.wrptr(a, program)?;
        }
        "_wpgmptr" => {
            let program = ensure_program_name(p, true)?;
            p.wrptr(a, program)?;
        }
        "_environ" => {
            let environ = ensure_environment(p, false)?;
            p.wrptr(a, environ)?;
        }
        "_wenviron" => {
            let environ = ensure_environment(p, true)?;
            p.wrptr(a, environ)?;
        }
        "__initenv" => {
            let environ = ensure_initial_environment(p, false)?;
            p.wrptr(a, environ)?;
        }
        "__winitenv" => {
            let environ = ensure_initial_environment(p, true)?;
            p.wrptr(a, environ)?;
        }
        "_iob" => {
            ensure_iob(p)?;
            // `_iob` is an array of FILE, not a pointer: callers take its address.
            return Ok(p.extra.iob);
        }
        "__mb_cur_max" => {
            p.wr32(a, 1)?;
        }
        "_HUGE" => {
            // The installed VC80 CRT exports the IEEE-754 positive infinity value here.
            p.wr64(a, f64::INFINITY.to_bits())?;
        }
        "_adjust_fdiv" => {
            // VC80's startup test sets this to IsProcessorFeaturePresent(0), which is
            // false for the emulated CPU. A zero cell therefore means no FDIV correction.
            p.wr32(a, 0)?;
        }
        "_winmajor" => {
            p.wr32(a, 10)?;
        }
        _ => {}
    }
    Ok(a)
}

fn ensure_iob(p: &mut Process) -> Result<u64, Stop> {
    if p.extra.iob == 0 {
        let a = kernel32::alloc_block(p, FILE_SIZE * 3, true);
        p.extra.iob = a;
        p.extra.files.insert(
            a,
            CFile {
                kind: FileKind::Stdin,
                eof: false,
                err: false,
                ungot: None,
            },
        );
        p.extra.files.insert(
            a + FILE_SIZE,
            CFile {
                kind: FileKind::Stdout,
                eof: false,
                err: false,
                ungot: None,
            },
        );
        p.extra.files.insert(
            a + 2 * FILE_SIZE,
            CFile {
                kind: FileKind::Stderr,
                eof: false,
                err: false,
                ungot: None,
            },
        );
    }
    Ok(p.extra.iob)
}

fn get_stream_buffer_pointers(p: &mut Process) -> R {
    let stream = p.arg(0);
    if stream == 0 {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(22);
    }
    for (output, field) in [
        (p.arg(1), stream.wrapping_add(8)),
        (p.arg(2), stream),
        (p.arg(3), stream.wrapping_add(0x10)),
    ] {
        if output != 0 {
            p.wr64(output, field)?;
        }
    }
    Ok(0)
}

/// Build argc/argv from the command line (once).
fn ensure_args(p: &mut Process) -> Result<(), Stop> {
    if p.extra.argv != 0 {
        return Ok(());
    }
    let args = split_command_line(&p.cmdline);
    p.extra.argc = args.len() as u32;
    let width = guest_pointer_width(p);
    let arr = kernel32::alloc_block(p, width * (args.len() as u64 + 1), true);
    for (i, a) in args.iter().enumerate() {
        let s = kernel32::alloc_block(p, a.len() as u64 + 1, true);
        p.write_cstr(s, a)?;
        p.wrptr(arr + width * i as u64, s)?;
    }
    p.extra.argv = arr;
    kernel32::command_line_a(p)?;
    Ok(())
}

fn ensure_wide_args(p: &mut Process) -> Result<(), Stop> {
    if p.extra.wargv != 0 {
        return Ok(());
    }
    let args = split_command_line(&p.cmdline);
    p.extra.argc = args.len() as u32;
    let width = guest_pointer_width(p);
    let arr = kernel32::alloc_block(p, width * (args.len() as u64 + 1), true);
    for (i, arg) in args.iter().enumerate() {
        let units = arg.encode_utf16().count() as u64;
        let string = kernel32::alloc_block(p, (units + 1) * 2, true);
        p.write_wstr(string, arg)?;
        p.wrptr(arr + width * i as u64, string)?;
    }
    p.extra.wargv = arr;
    kernel32::command_line_w(p)?;
    Ok(())
}

fn ensure_program_name(p: &mut Process, wide: bool) -> Result<u64, Stop> {
    let cached = if wide {
        p.extra.program_w
    } else {
        p.extra.program_a
    };
    if cached != 0 {
        return Ok(cached);
    }
    let image_path = if p.image_path.is_empty() {
        split_command_line(&p.cmdline)
            .into_iter()
            .next()
            .unwrap_or_default()
    } else {
        p.image_path.clone()
    };
    let string = if wide {
        let units = image_path.encode_utf16().count() as u64;
        let string = kernel32::alloc_block(p, (units + 1) * 2, true);
        p.write_wstr(string, &image_path)?;
        p.extra.program_w = string;
        string
    } else {
        let string = kernel32::alloc_block(p, image_path.len() as u64 + 1, true);
        p.write_cstr(string, &image_path)?;
        p.extra.program_a = string;
        string
    };
    Ok(string)
}

fn ensure_environment(p: &mut Process, wide: bool) -> Result<u64, Stop> {
    let cached = if wide { p.extra.env_w } else { p.extra.env_a };
    if cached != 0 {
        return Ok(cached);
    }
    let entries = p.env.clone();
    let array = build_environment_array(p, &entries, wide)?;
    if wide {
        p.extra.env_w = array;
    } else {
        p.extra.env_a = array;
    }
    Ok(array)
}

fn ensure_initial_environment(p: &mut Process, wide: bool) -> Result<u64, Stop> {
    let cached = if wide {
        p.extra.initial_env_w
    } else {
        p.extra.initial_env_a
    };
    if cached != 0 {
        return Ok(cached);
    }
    let entries = p.initial_env.clone().unwrap_or_else(|| p.env.clone());
    let array = build_environment_array(p, &entries, wide)?;
    if wide {
        p.extra.initial_env_w = array;
    } else {
        p.extra.initial_env_a = array;
    }
    Ok(array)
}

fn build_environment_array(
    p: &mut Process,
    environment: &[(String, String)],
    wide: bool,
) -> Result<u64, Stop> {
    let entries: Vec<String> = environment
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    let width = guest_pointer_width(p);
    let array = kernel32::alloc_block(p, width * (entries.len() as u64 + 1), true);
    for (index, entry) in entries.iter().enumerate() {
        let string = if wide {
            let units = entry.encode_utf16().count() as u64;
            let string = kernel32::alloc_block(p, (units + 1) * 2, true);
            p.write_wstr(string, entry)?;
            string
        } else {
            let string = kernel32::alloc_block(p, entry.len() as u64 + 1, true);
            p.write_cstr(string, entry)?;
            string
        };
        p.wrptr(array + width * index as u64, string)?;
    }
    Ok(array)
}

pub(super) fn refresh_environment(p: &mut Process) -> Result<(), Stop> {
    let narrow_cell = p.extra.data_cells.get("_environ").copied();
    let wide_cell = p.extra.data_cells.get("_wenviron").copied();
    p.extra.env_a = 0;
    p.extra.env_w = 0;
    if let Some(cell) = narrow_cell {
        let environ = ensure_environment(p, false)?;
        p.wrptr(cell, environ)?;
    }
    if let Some(cell) = wide_cell {
        let environ = ensure_environment(p, true)?;
        p.wrptr(cell, environ)?;
    }
    Ok(())
}

fn crt_main_args(p: &mut Process, wide: bool) -> R {
    let name = if wide {
        "__wgetmainargs"
    } else {
        "__getmainargs"
    };
    if p.arg(3) != 0 {
        return Err(Stop::Crash(format!(
            "{name}: wildcard argument expansion requested but not translated"
        )));
    }
    let argv = if wide {
        ensure_wide_args(p)?;
        p.extra.wargv
    } else {
        ensure_args(p)?;
        p.extra.argv
    };
    let environ = ensure_environment(p, wide)?;
    p.wr32(p.arg(0), p.extra.argc)?;
    p.wrptr(p.arg(1), argv)?;
    p.wrptr(p.arg(2), environ)?;
    Ok(0)
}

fn pointer_data_cell(p: &mut Process, key: &str, value: u64) -> R {
    let cell = data_cell(p, key, 8);
    p.wrptr(cell, value)?;
    Ok(cell)
}

fn setlocale(p: &mut Process) -> R {
    let category = p.arg(0) as i32;
    let locale = p.arg(1);
    if !(0..=5).contains(&category) {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }
    if locale != 0 {
        let requested = p
            .mem
            .read_cstr(locale, 256)
            .map_err(|e| Stop::Crash(format!("{e:?}")))?;
        if !requested.is_empty() && requested != b"C" {
            let errno = errno_cell(p);
            p.wr32(errno, 22)?; // EINVAL
            return Ok(0);
        }
    }
    let key = "setlocale:C";
    if let Some(&name) = p.extra.data_cells.get(key) {
        return Ok(name);
    }
    let name = kernel32::alloc_block(p, 2, true);
    p.write_cstr(name, "C")?;
    p.extra.data_cells.insert(key.to_string(), name);
    Ok(name)
}

fn wsetlocale(p: &mut Process) -> R {
    let category = p.arg(0) as i32;
    let locale = p.arg(1);
    if !(0..=5).contains(&category) {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }
    if locale != 0 {
        let requested = p.read_wstr(locale)?;
        if !requested.is_empty() && requested != "C" {
            let errno = errno_cell(p);
            p.wr32(errno, 22)?; // EINVAL
            return Ok(0);
        }
    }
    let key = "wsetlocale:C";
    if let Some(&name) = p.extra.data_cells.get(key) {
        return Ok(name);
    }
    let name = kernel32::alloc_block(p, 4, true);
    p.write_wstr(name, "C")?;
    p.extra.data_cells.insert(key.to_string(), name);
    Ok(name)
}

fn crt_configthreadlocale(p: &mut Process) -> R {
    let mode = p.arg(0) as i32;
    let key = format!("crt:thread_locale_mode:{}", p.current_thread_id);
    let previous = p.extra.data_cells.get(&key).copied().unwrap_or(0) as u32;
    match mode {
        -1 => Ok(previous as u64),
        0 | 1 => {
            p.extra.data_cells.insert(key, mode as u64);
            Ok(previous as u64)
        }
        _ => {
            let errno = errno_cell(p);
            p.wr32(errno, 22)?; // EINVAL
            Ok(u32::MAX as u64)
        }
    }
}

fn crt_setmbcp(p: &mut Process) -> R {
    let requested = p.arg(0) as i32;
    if !matches!(requested, 0 | 1252) {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!(
                "_setmbcp code page {requested} (only the active Windows-1252 page is modeled)"
            ),
        }
        .record(p));
    }
    p.extra
        .data_cells
        .insert("crt:multibyte_code_page".into(), 1252);
    Ok(0)
}

fn msvc_query_perf_counter(p: &mut Process) -> R {
    Ok(kernel32::query_performance_counter_value(p))
}

/// CommandLineToArgv-style splitting (quotes and backslash rules simplified to the common case).
pub fn split_command_line(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut inq = false;
    let mut any = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                inq = !inq;
                any = true;
            }
            c if c.is_whitespace() && !inq => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn stream_write(p: &mut Process, fp: u64, data: &[u8]) -> Result<usize, Stop> {
    ensure_iob(p)?;
    let Some(f) = p.extra.files.get_mut(&fp) else {
        return Ok(0);
    };
    let mut change = None;
    match &mut f.kind {
        FileKind::Stdout => p.stdout.extend_from_slice(data),
        FileKind::Stderr => p.stderr.extend_from_slice(data),
        FileKind::Stdin => return Ok(0),
        FileKind::Vfs {
            path,
            pos,
            read: _,
            write,
        } => {
            if !*write || p.vfs.is_readonly(path) {
                f.err = true;
                return Ok(0);
            }
            let (path, at) = (path.clone(), *pos);
            let Some(file) = p.vfs.get_mut(&path) else {
                f.err = true;
                return Ok(0);
            };
            let old_len = file.len();
            if file.len() < at + data.len() {
                file.resize(at + data.len(), 0);
            }
            file[at..at + data.len()].copy_from_slice(data);
            let mut filter = 0x10; // FILE_NOTIFY_CHANGE_LAST_WRITE
            if old_len != file.len() {
                filter |= 0x08; // FILE_NOTIFY_CHANGE_SIZE
            }
            if !data.is_empty() {
                change = Some((path, filter));
            }
            *pos = at.saturating_add(data.len());
        }
    }
    if let Some((path, filter)) = change {
        p.notify_vfs_change(&path, filter);
    }
    Ok(data.len())
}

fn stream_read(p: &mut Process, fp: u64, len: usize) -> Result<Vec<u8>, Stop> {
    ensure_iob(p)?;
    let Some(f) = p.extra.files.get(&fp) else {
        return Ok(Vec::new());
    };
    match &f.kind {
        FileKind::Stdin => {
            let mut out =
                Vec::with_capacity(len.min(p.stdin.len() + usize::from(f.ungot.is_some())));
            if let Some(byte) = f.ungot {
                out.push(byte);
            }
            let from_stdin = len.saturating_sub(out.len()).min(p.stdin.len());
            out.extend(p.stdin.drain(..from_stdin));
            if let Some(f) = p.extra.files.get_mut(&fp) {
                f.ungot = None;
                f.eof = out.len() < len;
            }
            Ok(out)
        }
        FileKind::Vfs {
            path, pos, read, ..
        } => {
            if !*read {
                if let Some(file) = p.extra.files.get_mut(&fp) {
                    file.err = true;
                }
                return Ok(Vec::new());
            }
            let (path, pos, ungot) = (path.clone(), *pos, f.ungot);
            let disk_len = len.saturating_sub(usize::from(ungot.is_some()));
            let disk_bytes = p
                .vfs
                .read_range(&path, pos as u64, disk_len)
                .map_err(|error| Stop::Crash(format!("file read failed for {path}: {error:?}")))?;
            let mut out =
                Vec::with_capacity(len.min(disk_bytes.len() + usize::from(ungot.is_some())));
            if let Some(byte) = ungot {
                out.push(byte);
            }
            out.extend_from_slice(&disk_bytes);
            if let Some(f) = p.extra.files.get_mut(&fp) {
                if f.ungot == ungot {
                    f.ungot = None;
                }
                if let FileKind::Vfs { pos, .. } = &mut f.kind {
                    *pos = pos.saturating_add(disk_bytes.len());
                }
                f.eof = out.len() < len;
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

fn stream_read_byte(p: &mut Process, fp: u64) -> Result<Option<u8>, Stop> {
    Ok(stream_read(p, fp, 1)?.first().copied())
}

fn do_printf(p: &mut Process, fmt: u64, args: &mut dyn Args, wide: bool) -> Result<Vec<u8>, Stop> {
    let f = if wide {
        p.read_wstr(fmt)?.into_bytes()
    } else {
        p.mem
            .read_cstr(fmt, 1 << 20)
            .map_err(|e| Stop::Crash(format!("{e:?}")))?
    };
    printf::format(p, &f, args, wide)
}

fn store_formatted(
    p: &mut Process,
    buf: u64,
    cap: Option<u64>,
    out: &[u8],
    wide: bool,
) -> Result<(), Stop> {
    if buf == 0 {
        return Ok(());
    }
    if wide {
        let w: Vec<u16> = String::from_utf8_lossy(out).encode_utf16().collect();
        let n = match cap {
            Some(c) if c == 0 => return Ok(()),
            Some(c) => (c as usize - 1).min(w.len()),
            None => w.len(),
        };
        let mut raw: Vec<u8> = w[..n].iter().flat_map(|c| c.to_le_bytes()).collect();
        raw.extend([0, 0]);
        p.write_bytes(buf, &raw)
    } else {
        let n = match cap {
            Some(0) => return Ok(()),
            Some(c) => (c as usize - 1).min(out.len()),
            None => out.len(),
        };
        let mut v = out[..n].to_vec();
        v.push(0);
        p.write_bytes(buf, &v)
    }
}

struct CrtX86VaList(u64);

impl Args for CrtX86VaList {
    fn next(&mut self, p: &Process) -> Result<u64, Stop> {
        let value = p.rd32(self.0)? as u64;
        self.0 = self
            .0
            .checked_add(4)
            .ok_or_else(|| Stop::Crash("x86 CRT va_list pointer overflow".into()))?;
        Ok(value)
    }
}

fn crt_secure_vsnprintf(p: &mut Process, wide: bool, va_list: bool) -> R {
    let (buffer, capacity) = (p.arg(0), p.arg(1) as usize);
    let (count, format, arguments) = if wide {
        // _vswprintf_c_l(buffer, size, format, locale, va_list)
        if p.arg(3) != 0 {
            return Err(Stop::Unimplemented {
                dll: "crt".into(),
                name: "_vswprintf_c_l with non-null locale handle".into(),
            }
            .record(p));
        }
        (usize::MAX, p.arg(2), p.arg(4))
    } else if va_list {
        (p.arg(2) as usize, p.arg(3), p.arg(4))
    } else {
        (p.arg(2) as usize, p.arg(3), 0)
    };
    let function = if wide {
        "_vswprintf_c_l"
    } else if va_list {
        "_vsnprintf_s"
    } else {
        "_snprintf_s"
    };
    if buffer == 0 || capacity == 0 || format == 0 {
        if buffer != 0 && capacity != 0 {
            if wide {
                p.wr16(buffer, 0)?;
            } else {
                p.wr8(buffer, 0)?;
            }
        }
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        crt_invalid_parameter(p, function)?;
        return Ok(u32::MAX as u64);
    }
    if capacity > (1 << 20) {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("{function} capacity {capacity} exceeds the translated output limit"),
        }
        .record(p));
    }
    let mut args: Box<dyn Args> = if va_list && p.cpu.mode == CpuMode::X86_32 {
        Box::new(CrtX86VaList(arguments))
    } else if va_list {
        Box::new(VaList(arguments))
    } else {
        Box::new(SlotArgs(4))
    };
    let output = do_printf(p, format, args.as_mut(), wide)?;
    let output_length = len_for(&output, wide) as usize;
    let truncate_count = if p.cpu.mode == CpuMode::X86_32 {
        u32::MAX as u64
    } else {
        u64::MAX
    };
    let sentinel_truncate = !wide && count as u64 == truncate_count;
    let writable = if sentinel_truncate {
        capacity - 1
    } else {
        count.min(capacity - 1)
    };
    if output_length <= writable {
        store_formatted(p, buffer, Some((writable + 1) as u64), &output, wide)?;
        return Ok(output_length as u64);
    }

    if sentinel_truncate || wide {
        store_formatted(p, buffer, Some((capacity) as u64), &output, wide)?;
        let errno = errno_cell(p);
        p.wr32(errno, 34)?; // ERANGE
        return Ok(u32::MAX as u64);
    }

    if wide {
        p.wr16(buffer, 0)?;
    } else {
        p.wr8(buffer, 0)?;
    }
    let errno = errno_cell(p);
    p.wr32(errno, 34)?; // ERANGE
    crt_invalid_parameter(p, function)?;
    Ok(u32::MAX as u64)
}

fn len_for(out: &[u8], wide: bool) -> u64 {
    if wide {
        String::from_utf8_lossy(out).encode_utf16().count() as u64
    } else {
        out.len() as u64
    }
}

/// Initialize the MSVC `basic_ios` subobject using the layout emitted by MSVCP140's
/// default constructor. Its synthetic vtable leaves the destructor as an explicit import trap.
fn msvc_basic_ios_default(p: &mut Process, key: &str, destructor: &str, wide: bool) -> R {
    let vtable = data_cell(p, key, 0x20);
    if p.rd64(vtable)? == 0 {
        let destructor = p
            .thunk_for("msvcp", destructor, true)
            .expect("creating an MSVCP destructor thunk");
        p.wr64(vtable, destructor)?;
    }

    let this = p.arg(0);
    p.wr64(this, vtable)?;
    p.wr64(this + 0x08, 0)?;
    p.wr64(this + 0x10, 0)?;
    p.wr32(this + 0x18, 0)?;
    for offset in [0x20, 0x28, 0x30, 0x38, 0x40, 0x48, 0x50] {
        p.wr64(this + offset, 0)?;
    }
    if wide {
        p.wr16(this + 0x58, 0)?;
    } else {
        p.wr8(this + 0x58, 0)?;
    }
    Ok(this)
}

/// MSVC's default `basic_streambuf<char>` `_Lock` and `_Unlock` virtuals are
/// no-ops. Preserve RAX just as their native `ret 0` bodies do.
fn msvc_basic_streambuf_char_lock_noop(p: &mut Process) -> R {
    Ok(p.cpu.gpr[RAX])
}

/// `sputn` is a wrapper around the object's virtual `xsputn` method.
fn msvc_basic_streambuf_char_sputn(p: &mut Process) -> R {
    let this = p.arg(0);
    let vtable = p.rd64(this)?;
    let xsputn = p.rd64(vtable + 9 * 8)?;
    p.call_guest(xsputn, &[this, p.arg(1), p.cpu.gpr[R8]])
}

/// `sputc` writes directly when the put area has room and otherwise calls `overflow`.
fn msvc_basic_streambuf_char_sputc(p: &mut Process) -> R {
    let this = p.arg(0);
    let character = p.arg(1) as u8;
    let put_next_cell = p.rd64(this + 0x40)?;
    let put_count_cell = p.rd64(this + 0x58)?;
    let put_next = p.rd64(put_next_cell)?;
    let put_count = if put_next == 0 {
        0
    } else {
        p.rd32(put_count_cell)? as i32
    };
    if put_next != 0 && put_count > 0 {
        p.wr8(put_next, character)?;
        p.wr64(put_next_cell, put_next.wrapping_add(1))?;
        p.wr32(put_count_cell, (put_count - 1) as u32)?;
        return Ok(character as u64);
    }

    let vtable = p.rd64(this)?;
    let overflow = p.rd64(vtable + 3 * 8)?;
    p.call_guest(overflow, &[this, character as u64])
}

/// MSVC's base `xsputn` copies to the current put area and calls virtual
/// `overflow` when that area is exhausted.
fn msvc_basic_streambuf_xsputn(p: &mut Process, char_type: &str, char_size: usize) -> R {
    let this = p.arg(0);
    let mut remaining = p.cpu.gpr[R8] as i64;
    if remaining <= 0 {
        return Ok(0);
    }

    let expected_vtable = data_cell(
        p,
        &format!("msvcp:basic_streambuf:{char_type}:vtable"),
        0x80,
    );
    let vtable = p.rd64(this)?;
    let source = p.arg(1);
    let put_next_cell = p.rd64(this + 0x40)?;
    let put_count_cell = p.rd64(this + 0x58)?;
    let mut written = 0u64;

    while remaining > 0 {
        let put_next = p.rd64(put_next_cell)?;
        let put_count = if put_next == 0 {
            0
        } else {
            p.rd32(put_count_cell)? as i32
        };
        if put_next != 0 && put_count > 0 {
            let count = remaining.min(put_count as i64) as usize;
            let byte_count = count * char_size;
            let bytes =
                p.read_bytes(source.wrapping_add(written * char_size as u64), byte_count)?;
            p.write_bytes(put_next, &bytes)?;
            written += count as u64;
            remaining -= count as i64;
            p.wr64(
                put_next_cell,
                put_next.wrapping_add((count * char_size) as u64),
            )?;
            p.wr32(put_count_cell, (put_count - count as i32) as u32)?;
        } else {
            let source_offset = source.wrapping_add(written * char_size as u64);
            let character = if char_size == 1 {
                p.rd8(source_offset)? as u32
            } else {
                p.rd16(source_offset)? as u32
            };
            if vtable == expected_vtable {
                // The base overflow method returns EOF when no put area exists.
                break;
            }
            let overflow = p.rd64(vtable + 3 * 8)?;
            let result = p.call_guest(overflow, &[this, character as u64])?;
            let failed = if char_size == 1 {
                result as u32 == u32::MAX
            } else {
                result as u16 == u16::MAX
            };
            if failed {
                break;
            }
            written += 1;
            remaining -= 1;
        }
    }

    Ok(written)
}

fn msvc_basic_streambuf_char_xsputn(p: &mut Process) -> R {
    msvc_basic_streambuf_xsputn(p, "char", 1)
}

fn msvc_basic_streambuf_wchar_xsputn(p: &mut Process) -> R {
    msvc_basic_streambuf_xsputn(p, "wchar", 2)
}

/// The base `basic_streambuf` underflow method reports EOF when no input source
/// has been installed.
fn msvc_basic_streambuf_underflow(_p: &mut Process, char_size: usize) -> R {
    Ok(if char_size == 1 {
        u32::MAX as u64
    } else {
        u16::MAX as u64
    })
}

fn msvc_basic_streambuf_char_underflow(p: &mut Process) -> R {
    msvc_basic_streambuf_underflow(p, 1)
}

fn msvc_basic_streambuf_wchar_underflow(p: &mut Process) -> R {
    msvc_basic_streambuf_underflow(p, 2)
}

fn msvc_streambuf_get_area(p: &Process, this: u64) -> Result<(u64, u64, u64, i32), Stop> {
    let next_cell = p.rd64(this + 0x38)?;
    let next = if next_cell == 0 {
        0
    } else {
        p.rd64(next_cell)?
    };
    let count_cell = p.rd64(this + 0x50)?;
    let count = if next == 0 || count_cell == 0 {
        0
    } else {
        p.rd32(count_cell)? as i32
    };
    Ok((next_cell, count_cell, next, count))
}

fn msvc_basic_streambuf_uflow(p: &mut Process, char_size: usize) -> R {
    let this = p.arg(0);
    let eof = if char_size == 1 {
        u32::MAX as u64
    } else {
        u16::MAX as u64
    };
    let (next_cell, count_cell, next, count) = msvc_streambuf_get_area(p, this)?;
    if next != 0 && count > 0 {
        let character = if char_size == 1 {
            p.rd8(next)? as u64
        } else {
            p.rd16(next)? as u64
        };
        p.wr64(
            next_cell,
            next.checked_add(char_size as u64)
                .ok_or_else(|| Stop::Crash("streambuf get pointer overflow".into()))?,
        )?;
        p.wr32(count_cell, (count - 1) as u32)?;
        return Ok(character);
    }

    // The base uflow delegates to virtual underflow. A derived streambuf can
    // populate its get area there; the base underflow above reports EOF.
    let vtable = p.rd64(this)?;
    let underflow = p.rd64(vtable + 6 * 8)?;
    let character = p.call_guest(underflow, &[this])?;
    if character == eof {
        return Ok(eof);
    }

    let (next_cell, count_cell, next, count) = msvc_streambuf_get_area(p, this)?;
    if next == 0 || count <= 0 {
        return Err(Stop::Crash(
            "streambuf underflow returned a character without an available get position".into(),
        ));
    }
    let buffered = if char_size == 1 {
        p.rd8(next)? as u64
    } else {
        p.rd16(next)? as u64
    };
    p.wr64(
        next_cell,
        next.checked_add(char_size as u64)
            .ok_or_else(|| Stop::Crash("streambuf get pointer overflow".into()))?,
    )?;
    p.wr32(count_cell, (count - 1) as u32)?;
    Ok(buffered)
}

fn msvc_basic_streambuf_char_uflow(p: &mut Process) -> R {
    msvc_basic_streambuf_uflow(p, 1)
}

fn msvc_basic_streambuf_wchar_uflow(p: &mut Process) -> R {
    msvc_basic_streambuf_uflow(p, 2)
}

fn msvc_basic_streambuf_xsgetn(p: &mut Process, char_size: usize) -> R {
    let this = p.arg(0);
    let destination = p.arg(1);
    let mut remaining = p.cpu.gpr[R8] as i64;
    if remaining <= 0 {
        return Ok(0);
    }

    let mut copied = 0u64;
    while remaining > 0 {
        let (next_cell, count_cell, next, count) = msvc_streambuf_get_area(p, this)?;
        if next != 0 && count > 0 {
            let elements = remaining.min(count as i64) as usize;
            let byte_count = elements
                .checked_mul(char_size)
                .ok_or_else(|| Stop::Crash("streambuf read size overflow".into()))?;
            let source = p.read_bytes(next, byte_count)?;
            let destination_offset = copied
                .checked_mul(char_size as u64)
                .and_then(|offset| destination.checked_add(offset))
                .ok_or_else(|| Stop::Crash("streambuf destination overflow".into()))?;
            p.write_bytes(destination_offset, &source)?;
            let advance = byte_count as u64;
            p.wr64(
                next_cell,
                next.checked_add(advance)
                    .ok_or_else(|| Stop::Crash("streambuf get pointer overflow".into()))?,
            )?;
            p.wr32(count_cell, (count - elements as i32) as u32)?;
            copied += elements as u64;
            remaining -= elements as i64;
            continue;
        }

        // The base xsgetn repeatedly calls virtual uflow once its buffered
        // input is exhausted, preserving behavior for derived streambufs.
        let vtable = p.rd64(this)?;
        let uflow = p.rd64(vtable + 7 * 8)?;
        let character = p.call_guest(uflow, &[this])?;
        let eof = if char_size == 1 {
            u32::MAX as u64
        } else {
            u16::MAX as u64
        };
        if character == eof {
            break;
        }
        let destination_offset = copied
            .checked_mul(char_size as u64)
            .and_then(|offset| destination.checked_add(offset))
            .ok_or_else(|| Stop::Crash("streambuf destination overflow".into()))?;
        if char_size == 1 {
            p.wr8(destination_offset, character as u8)?;
        } else {
            p.wr16(destination_offset, character as u16)?;
        }
        copied += 1;
        remaining -= 1;
    }

    Ok(copied)
}

fn msvc_basic_streambuf_char_xsgetn(p: &mut Process) -> R {
    msvc_basic_streambuf_xsgetn(p, 1)
}

fn msvc_basic_streambuf_wchar_xsgetn(p: &mut Process) -> R {
    msvc_basic_streambuf_xsgetn(p, 2)
}

/// The base `basic_streambuf<char>::sync()` succeeds without doing anything.
fn msvc_basic_streambuf_char_sync(_p: &mut Process) -> R {
    Ok(0)
}

fn msvc_basic_streambuf_showmanyc(_p: &mut Process) -> R {
    Ok(0)
}

fn msvc_basic_streambuf_setbuf(p: &mut Process) -> R {
    Ok(p.arg(0))
}

fn msvc_basic_streambuf_imbue(_p: &mut Process) -> R {
    Ok(0)
}

fn msvc_standard_streambuf_kind(p: &Process, streambuf: u64) -> Result<bool, Stop> {
    if p.extra.data_cells.get(MSVC_STD_COUT_BUFFER).copied() == Some(streambuf) {
        Ok(false)
    } else if p.extra.data_cells.get(MSVC_STD_CERR_BUFFER).copied() == Some(streambuf) {
        Ok(true)
    } else {
        Err(Stop::Unimplemented {
            dll: "msvcp".into(),
            name: "standard ostream streambuf is not a registered global".into(),
        })
    }
}

fn msvc_standard_streambuf_write(
    p: &mut Process,
    streambuf: u64,
    bytes: &[u8],
) -> Result<(), Stop> {
    if msvc_standard_streambuf_kind(p, streambuf)? {
        p.stderr.extend_from_slice(bytes);
    } else {
        p.stdout.extend_from_slice(bytes);
    }
    Ok(())
}

fn msvc_standard_streambuf_char_overflow(p: &mut Process) -> R {
    let streambuf = p.arg(0);
    let character = p.arg(1) as u32;
    if character == u32::MAX {
        return Ok(u32::MAX as u64);
    }
    msvc_standard_streambuf_write(p, streambuf, &[character as u8])?;
    Ok(character as u8 as u64)
}

fn msvc_standard_streambuf_xsputn(p: &mut Process) -> R {
    let streambuf = p.arg(0);
    let count = p.cpu.gpr[R8] as i64;
    if count <= 0 {
        return Ok(0);
    }
    let bytes = p.read_bytes(p.arg(1), count as usize)?;
    msvc_standard_streambuf_write(p, streambuf, &bytes)?;
    Ok(count as u64)
}

fn msvc_standard_streambuf_sync(_p: &mut Process) -> R {
    Ok(0)
}

fn msvc_standard_streambuf_init(p: &mut Process, streambuf: u64) -> Result<(), Stop> {
    let vtable = data_cell(p, MSVC_STD_STREAMBUF_VTABLE, 0x80);
    let unsupported = p
        .thunk_for(
            "msvcp",
            "__anywasm_unimplemented_basic_streambuf_char_virtual",
            true,
        )
        .expect("creating an explicit MSVCP streambuf virtual-call trap");
    for offset in (0..0x80).step_by(8) {
        p.wr64(vtable + offset, unsupported)?;
    }
    for (offset, name) in [
        (
            0,
            "??1?$basic_streambuf@DU?$char_traits@D@std@@@std@@UEAA@XZ",
        ),
        (8, "__anywasm_basic_streambuf_char_lock_noop"),
        (16, "__anywasm_basic_streambuf_char_lock_noop"),
        (3 * 8, "__anywasm_standard_streambuf_char_overflow"),
        (
            5 * 8,
            "?showmanyc@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAA_JXZ",
        ),
        (9 * 8, "__anywasm_standard_streambuf_xsputn"),
        (
            12 * 8,
            "?setbuf@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAAPEAV12@PEAD_J@Z",
        ),
        (13 * 8, "__anywasm_standard_streambuf_sync"),
        (
            14 * 8,
            "?imbue@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAAXAEBVlocale@2@@Z",
        ),
    ] {
        let target = p
            .thunk_for("msvcp", name, true)
            .expect("creating an MSVCP standard streambuf thunk");
        p.wr64(vtable + offset, target)?;
    }

    let locale = msvc_locale_value(p, MSVC_STD_LOCALE)?;

    p.wr64(streambuf, vtable)?;
    p.wr64(streambuf + 0x08, 0)?;
    p.wr64(streambuf + 0x10, 0)?;
    p.wr64(streambuf + 0x18, streambuf + 0x08)?;
    p.wr64(streambuf + 0x20, streambuf + 0x10)?;
    p.wr64(streambuf + 0x28, 0)?;
    p.wr64(streambuf + 0x30, 0)?;
    p.wr64(streambuf + 0x38, streambuf + 0x28)?;
    p.wr64(streambuf + 0x40, streambuf + 0x30)?;
    p.wr64(streambuf + 0x48, 0)?;
    p.wr64(streambuf + 0x50, streambuf + 0x48)?;
    p.wr64(streambuf + 0x58, streambuf + 0x4c)?;
    p.wr64(streambuf + 0x60, locale)?;
    Ok(())
}

fn msvc_standard_ostream_init(
    p: &mut Process,
    stream: u64,
    streambuf: u64,
    unitbuf: bool,
    tied_to: u64,
) -> Result<(), Stop> {
    let vtable = data_cell(p, MSVC_STD_OSTREAM_VTABLE, 0x80);
    let unsupported = p
        .thunk_for(
            "msvcp",
            "__anywasm_unimplemented_basic_iostream_char_virtual",
            true,
        )
        .expect("creating an explicit MSVCP ostream virtual-call trap");
    for offset in (0..0x80).step_by(8) {
        p.wr64(vtable + offset, unsupported)?;
    }
    let destructor = p
        .thunk_for(
            "msvcp",
            "??1?$basic_ios@DU?$char_traits@D@std@@@std@@UEAA@XZ",
            true,
        )
        .expect("creating an MSVCP basic_ios destructor thunk");
    p.wr64(vtable, destructor)?;

    let vbtable = data_cell(p, MSVC_STD_OSTREAM_VBTABLE, 12);
    p.wr32(vbtable, 0)?;
    p.wr32(vbtable + 4, 0x10)?;
    p.wr32(vbtable + 8, 0)?;
    p.wr64(stream, vbtable)?;
    p.wr64(stream + 0x08, 0)?;

    let ios = stream + 0x10;
    p.wr64(ios, vtable)?;
    p.wr64(ios + 0x08, 0)?;
    p.wr32(ios + 0x10, 0)?;
    p.wr32(ios + 0x14, 0)?;
    p.wr32(ios + 0x18, 0x201 | if unitbuf { 0x2 } else { 0 })?;
    p.wr64(ios + 0x20, 6)?;
    p.wr64(ios + 0x28, 0)?;
    p.wr64(ios + 0x30, 0)?;
    p.wr64(ios + 0x38, 0)?;
    let locale = msvc_locale_value(p, MSVC_STD_LOCALE)?;
    p.wr64(ios + 0x40, locale)?;
    p.wr64(ios + 0x48, streambuf)?;
    p.wr64(ios + 0x50, tied_to)?;
    p.wr8(ios + 0x58, 0x20)?;
    msvc_standard_streambuf_init(p, streambuf)
}

// MSVCP80 x86 member imports are thiscall: `this` is in ECX and explicit arguments remain on
// the guest stack. The installed DLL bodies confirm the PE32 basic_ios offsets below.
const MSVC80_X86_IOS_STATE: u64 = 0x08;
const MSVC80_X86_IOS_EXCEPT: u64 = 0x0c;
const MSVC80_X86_IOS_FLAGS: u64 = 0x10;
const MSVC80_X86_IOS_WIDTH: u64 = 0x18;
const MSVC80_X86_IOS_STREAMBUF: u64 = 0x28;
const MSVC80_X86_IOS_FILL: u64 = 0x30;

fn msvc80_x86_unimplemented(p: &mut Process, name: impl Into<String>) -> Stop {
    Stop::Unimplemented {
        dll: "msvcp".into(),
        name: name.into(),
    }
    .record(p)
}

fn msvc80_x86_this(p: &mut Process, name: &str) -> Result<u64, Stop> {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(msvc80_x86_unimplemented(
            p,
            format!("{name} requires the PE32 MSVCP80 thiscall ABI"),
        ));
    }
    let this = p.cpu.gpr[RCX] as u32 as u64;
    if this == 0 {
        return Err(Stop::Crash(format!("{name}: null this pointer")));
    }
    Ok(this)
}

fn msvc80_x86_arg(p: &Process, index: usize) -> Result<u64, Stop> {
    let offset = 4u64
        .checked_add(
            (index as u64)
                .checked_mul(4)
                .ok_or_else(|| Stop::Crash("MSVCP80 x86 argument index overflow".into()))?,
        )
        .ok_or_else(|| Stop::Crash("MSVCP80 x86 stack offset overflow".into()))?;
    let address = (p.cpu.gpr[RSP] as u32 as u64)
        .checked_add(offset)
        .ok_or_else(|| Stop::Crash("MSVCP80 x86 stack address overflow".into()))?;
    Ok(p.rd32(address)? as u64)
}

fn msvc80_x86_call_this(
    p: &mut Process,
    target: u64,
    this: u64,
    args: &[u64],
) -> Result<u64, Stop> {
    let saved_ecx = p.cpu.gpr[RCX];
    p.cpu.gpr[RCX] = this as u32 as u64;
    let result = p.call_guest(target, args);
    p.cpu.gpr[RCX] = saved_ecx;
    result
}

fn msvc80_x86_ostream_ios(p: &Process, ostream: u64) -> Result<u64, Stop> {
    let vbtable = p.rd32(ostream)? as u64;
    if vbtable == 0 {
        return Err(Stop::Crash(format!(
            "MSVCP80 ostream {ostream:#x} has a null virtual-base table"
        )));
    }
    let displacement = p.rd32(vbtable + 4)? as i32;
    if displacement >= 0 {
        ostream
            .checked_add(displacement as u64)
            .ok_or_else(|| Stop::Crash("MSVCP80 ostream ios offset overflow".into()))
    } else {
        ostream
            .checked_sub(displacement.unsigned_abs() as u64)
            .ok_or_else(|| Stop::Crash("MSVCP80 ostream ios offset underflow".into()))
    }
}

fn msvc80_x86_streambuf(p: &Process, ios: u64) -> Result<u64, Stop> {
    let streambuf = p.rd32(ios + MSVC80_X86_IOS_STREAMBUF)? as u64;
    if streambuf == 0 {
        return Err(Stop::Crash("MSVCP80 basic_ios has a null streambuf".into()));
    }
    Ok(streambuf)
}

fn msvc80_x86_streambuf_kind(p: &Process, streambuf: u64) -> Option<u8> {
    if p.extra.data_cells.get(MSVC_STD_COUT_BUFFER_X86).copied() == Some(streambuf) {
        Some(1)
    } else if p.extra.data_cells.get(MSVC_STD_CERR_BUFFER_X86).copied() == Some(streambuf) {
        Some(2)
    } else if p.extra.data_cells.get(MSVC_STD_CIN_BUFFER_X86).copied() == Some(streambuf) {
        Some(0)
    } else {
        None
    }
}

fn msvc80_x86_streambuf_write(
    p: &mut Process,
    streambuf: u64,
    bytes: &[u8],
) -> Result<usize, Stop> {
    match msvc80_x86_streambuf_kind(p, streambuf) {
        Some(1) => p.stdout.extend_from_slice(bytes),
        Some(2) => p.stderr.extend_from_slice(bytes),
        Some(0) => {
            return Err(msvc80_x86_unimplemented(
                p,
                "writing through std::cin's streambuf",
            ));
        }
        Some(_) => {
            return Err(msvc80_x86_unimplemented(
                p,
                format!("writing through unknown standard streambuf {streambuf:#x}"),
            ));
        }
        None => {
            return Err(msvc80_x86_unimplemented(
                p,
                format!("basic_streambuf output for object {streambuf:#x}"),
            ));
        }
    }
    Ok(bytes.len())
}

fn msvc80_x86_streambuf_sputn(p: &mut Process, streambuf: u64, data: u64, count: i32) -> R {
    if count <= 0 {
        return Ok(0);
    }
    if msvc80_x86_streambuf_kind(p, streambuf).is_some() {
        if data == 0 {
            return Err(Stop::Crash(
                "basic_streambuf::sputn received a null source".into(),
            ));
        }
        let bytes = p.read_bytes(data, count as usize)?;
        return Ok(msvc80_x86_streambuf_write(p, streambuf, &bytes)? as u64);
    }
    let vtable = p.rd32(streambuf)? as u64;
    let xsputn = if vtable == 0 {
        0
    } else {
        p.rd32(vtable + 0x20)? as u64
    };
    if xsputn == 0 {
        return Err(msvc80_x86_unimplemented(
            p,
            format!("basic_streambuf::sputn on {streambuf:#x} without an xsputn target"),
        ));
    }
    msvc80_x86_call_this(p, xsputn, streambuf, &[data, count as u32 as u64])
}

fn msvc80_x86_streambuf_sputc(p: &mut Process, streambuf: u64, character: u8) -> R {
    if msvc80_x86_streambuf_kind(p, streambuf).is_some() {
        msvc80_x86_streambuf_write(p, streambuf, &[character])?;
        return Ok(character as u64);
    }

    // VC80 sputc reads the put pointer/count via +0x24/+0x34 and dispatches overflow at vtable +4.
    let pptr_cell = p.rd32(streambuf + 0x24)? as u64;
    let pcount_cell = p.rd32(streambuf + 0x34)? as u64;
    let next = if pptr_cell == 0 {
        0
    } else {
        p.rd32(pptr_cell)? as u64
    };
    let count = if next == 0 || pcount_cell == 0 {
        0
    } else {
        p.rd32(pcount_cell)? as i32
    };
    if next != 0 && count > 0 {
        p.wr8(next, character)?;
        p.wr32(pptr_cell, next.wrapping_add(1) as u32)?;
        p.wr32(pcount_cell, (count - 1) as u32)?;
        return Ok(character as u64);
    }
    let vtable = p.rd32(streambuf)? as u64;
    let overflow = if vtable == 0 {
        0
    } else {
        p.rd32(vtable + 4)? as u64
    };
    if overflow == 0 {
        return Err(msvc80_x86_unimplemented(
            p,
            format!("basic_streambuf::sputc overflow for object {streambuf:#x}"),
        ));
    }
    msvc80_x86_call_this(p, overflow, streambuf, &[character as u64])
}

fn msvc80_x86_setstate(p: &mut Process, ios: u64, add: u32) -> Result<(), Stop> {
    let state = (p.rd32(ios + MSVC80_X86_IOS_STATE)? | add) & 0x17;
    p.wr32(ios + MSVC80_X86_IOS_STATE, state)?;
    if state & p.rd32(ios + MSVC80_X86_IOS_EXCEPT)? != 0 {
        return Err(msvc80_x86_unimplemented(
            p,
            "basic_ios transition throwing ios_base::failure",
        ));
    }
    Ok(())
}

fn msvc80_x86_ostream_flush(p: &mut Process, ostream: u64) -> Result<(), Stop> {
    let ios = msvc80_x86_ostream_ios(p, ostream)?;
    if p.rd32(ios + MSVC80_X86_IOS_STATE)? & 0x6 == 0 {
        let streambuf = msvc80_x86_streambuf(p, ios)?;
        let result = if msvc80_x86_streambuf_kind(p, streambuf).is_some() {
            0
        } else {
            let vtable = p.rd32(streambuf)? as u64;
            let sync = if vtable == 0 {
                0
            } else {
                p.rd32(vtable + 0x30)? as u64
            };
            if sync == 0 {
                return Err(msvc80_x86_unimplemented(
                    p,
                    format!("basic_ostream::flush sync for streambuf {streambuf:#x}"),
                ));
            }
            msvc80_x86_call_this(p, sync, streambuf, &[])? as u32 as i32
        };
        if result == -1 {
            msvc80_x86_setstate(p, ios, 0x4)?;
        }
    }
    Ok(())
}

fn msvc80_x86_ostream_osfx(p: &mut Process, ostream: u64) -> Result<(), Stop> {
    let ios = msvc80_x86_ostream_ios(p, ostream)?;
    if p.rd32(ios + MSVC80_X86_IOS_FLAGS)? & 0x2 != 0 {
        msvc80_x86_ostream_flush(p, ostream)?;
    }
    Ok(())
}

fn msvc80_x86_ostream_insert_i32(p: &mut Process, ostream: u64, value: i32) -> R {
    let ios = msvc80_x86_ostream_ios(p, ostream)?;
    if p.rd32(ios + MSVC80_X86_IOS_STATE)? != 0 {
        return Ok(ostream);
    }
    let streambuf = msvc80_x86_streambuf(p, ios)?;
    let flags = p.rd32(ios + MSVC80_X86_IOS_FLAGS)?;
    let basefield = flags & 0x0e00;
    let (base, upper) = match basefield {
        0x0400 => (8u32, false),
        0x0800 => (16u32, flags & 0x4 != 0),
        _ => (10u32, false),
    };
    let negative = value < 0 && base == 10;
    let mut magnitude = if value < 0 && base != 10 {
        (value as u32).wrapping_neg()
    } else {
        value.unsigned_abs()
    };
    let alphabet = if upper {
        b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
    } else {
        b"0123456789abcdefghijklmnopqrstuvwxyz"
    };
    let mut digits = [0u8; 32];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = alphabet[(magnitude % base) as usize];
        magnitude /= base;
        if magnitude == 0 {
            break;
        }
    }
    let mut body = Vec::with_capacity(36);
    if negative {
        body.push(b'-');
    } else if flags & 0x20 != 0 {
        body.push(b'+');
    }
    if flags & 0x8 != 0 {
        if base == 16 && digits[start] != b'0' {
            body.extend_from_slice(if upper { b"0X" } else { b"0x" });
        } else if base == 8 && digits[start] != b'0' {
            body.push(b'0');
        }
    }
    let prefix_len = body.len();
    body.extend_from_slice(&digits[start..]);
    let width = p.rd32(ios + MSVC80_X86_IOS_WIDTH)? as i32;
    p.wr32(ios + MSVC80_X86_IOS_WIDTH, 0)?;
    let padding = (width - body.len() as i32).max(0) as usize;
    if padding > 1_048_576 {
        return Err(msvc80_x86_unimplemented(
            p,
            "basic_ostream width above 1048576",
        ));
    }
    let fill = p.rd8(ios + MSVC80_X86_IOS_FILL)?;
    let left = flags & 0x40 != 0;
    let internal = !left && flags & 0x100 != 0;
    let mut output = Vec::with_capacity(body.len() + padding);
    if !left && !internal {
        output.resize(padding, fill);
        output.extend_from_slice(&body);
    } else if internal {
        output.extend_from_slice(&body[..prefix_len]);
        output.resize(prefix_len + padding, fill);
        output.extend_from_slice(&body[prefix_len..]);
    } else {
        output.extend_from_slice(&body);
        output.resize(body.len() + padding, fill);
    }
    if msvc80_x86_streambuf_write(p, streambuf, &output)? != output.len() {
        msvc80_x86_setstate(p, ios, 0x4)?;
    }
    if p.rd32(ios + MSVC80_X86_IOS_STATE)? == 0 {
        msvc80_x86_ostream_osfx(p, ostream)?;
    }
    Ok(ostream)
}

fn msvc80_x86_ostream_insert_int(p: &mut Process) -> R {
    let ostream = msvc80_x86_this(p, "basic_ostream::operator<<(int)")?;
    let value = msvc80_x86_arg(p, 0)? as u32 as i32;
    msvc80_x86_ostream_insert_i32(p, ostream, value)
}

fn msvc80_x86_istream_read(p: &mut Process) -> R {
    let istream = msvc80_x86_this(p, "basic_istream::read")?;
    let data = msvc80_x86_arg(p, 0)?;
    let count = msvc80_x86_arg(p, 1)? as u32 as i32;
    if count < 0 {
        return Err(msvc80_x86_unimplemented(
            p,
            "basic_istream::read with a negative count",
        ));
    }
    let ios = msvc80_x86_ostream_ios(p, istream)?;
    if p.rd32(ios + MSVC80_X86_IOS_STATE)? != 0 {
        return Ok(istream);
    }
    let streambuf = msvc80_x86_streambuf(p, ios)?;
    if msvc80_x86_streambuf_kind(p, streambuf) != Some(0) {
        return Err(msvc80_x86_unimplemented(
            p,
            format!("basic_istream::read through non-standard streambuf {streambuf:#x}"),
        ));
    }
    if count > 0 && data == 0 {
        return Err(Stop::Crash(
            "basic_istream::read received a null destination".into(),
        ));
    }
    let mut read = 0usize;
    while read < count as usize {
        let Some(byte) = p.stdin.pop_front() else {
            break;
        };
        p.wr8(data + read as u64, byte)?;
        read += 1;
    }
    if read != count as usize {
        msvc80_x86_setstate(p, ios, 0x1 | 0x2)?;
    }
    Ok(istream)
}

fn msvc80_x86_char_traits_length(p: &mut Process) -> R {
    let text = p.arg(0);
    if text == 0 {
        return Err(Stop::Crash(
            "char_traits<char>::length received a null pointer".into(),
        ));
    }
    Ok(p.read_cstr_raw(text)?.len() as u64)
}

fn msvc80_x86_char_traits_eof(_p: &mut Process) -> R {
    Ok(u32::MAX as u64)
}

fn msvc80_x86_char_traits_eq_int_type(p: &mut Process) -> R {
    let left = p.arg(0);
    let right = p.arg(1);
    if left == 0 || right == 0 {
        return Err(Stop::Crash(
            "char_traits<char>::eq_int_type received a null reference".into(),
        ));
    }
    Ok(u64::from(p.rd32(left)? == p.rd32(right)?))
}

fn msvc80_x86_basic_ios_setstate(p: &mut Process) -> R {
    let ios = msvc80_x86_this(p, "basic_ios::setstate")?;
    let state = msvc80_x86_arg(p, 0)? as u32;
    let _reraise = msvc80_x86_arg(p, 1)? != 0;
    msvc80_x86_setstate(p, ios, state)?;
    Ok(0)
}

fn msvc80_x86_basic_streambuf_sputc(p: &mut Process) -> R {
    let streambuf = msvc80_x86_this(p, "basic_streambuf::sputc")?;
    let character = msvc80_x86_arg(p, 0)? as u8;
    msvc80_x86_streambuf_sputc(p, streambuf, character)
}

fn msvc80_x86_basic_streambuf_sputn(p: &mut Process) -> R {
    let streambuf = msvc80_x86_this(p, "basic_streambuf::sputn")?;
    let data = msvc80_x86_arg(p, 0)?;
    let count = msvc80_x86_arg(p, 1)? as u32 as i32;
    if count < 0 {
        return Err(msvc80_x86_unimplemented(
            p,
            "basic_streambuf::sputn with a negative count",
        ));
    }
    msvc80_x86_streambuf_sputn(p, streambuf, data, count)
}

fn msvc80_x86_ostream_insert_manipulator(p: &mut Process) -> R {
    let ostream = msvc80_x86_this(p, "basic_ostream manipulator insertion")?;
    let manipulator = msvc80_x86_arg(p, 0)?;
    if manipulator == 0 {
        return Err(Stop::Crash(
            "basic_ostream manipulator insertion received a null function".into(),
        ));
    }
    p.call_guest(manipulator, &[ostream])
}

fn msvc80_x86_ostream_write(p: &mut Process) -> R {
    let ostream = msvc80_x86_this(p, "basic_ostream::write")?;
    let data = msvc80_x86_arg(p, 0)?;
    let count = msvc80_x86_arg(p, 1)? as u32 as i32;
    if count < 0 {
        return Err(msvc80_x86_unimplemented(
            p,
            "basic_ostream::write with a negative count",
        ));
    }
    let ios = msvc80_x86_ostream_ios(p, ostream)?;
    if p.rd32(ios + MSVC80_X86_IOS_STATE)? & 0x6 == 0 {
        let streambuf = msvc80_x86_streambuf(p, ios)?;
        let written = msvc80_x86_streambuf_sputn(p, streambuf, data, count)? as u32 as i32;
        if written != count {
            msvc80_x86_setstate(p, ios, 0x4)?;
        }
        if p.rd32(ios + MSVC80_X86_IOS_STATE)? == 0 {
            msvc80_x86_ostream_osfx(p, ostream)?;
        }
    }
    Ok(ostream)
}

fn msvc80_x86_ostream_flush_import(p: &mut Process) -> R {
    let ostream = msvc80_x86_this(p, "basic_ostream::flush")?;
    msvc80_x86_ostream_flush(p, ostream)?;
    Ok(ostream)
}

fn msvc80_x86_ostream_osfx_import(p: &mut Process) -> R {
    let ostream = msvc80_x86_this(p, "basic_ostream::_Osfx")?;
    msvc80_x86_ostream_osfx(p, ostream)?;
    Ok(0)
}

fn msvc80_x86_ostream_endl(p: &mut Process) -> R {
    let ostream = p.arg(0) as u32 as u64;
    if ostream == 0 {
        return Err(Stop::Crash(
            "std::endl received a null ostream reference".into(),
        ));
    }
    let ios = msvc80_x86_ostream_ios(p, ostream)?;
    if p.rd32(ios + MSVC80_X86_IOS_STATE)? & 0x6 == 0 {
        let streambuf = msvc80_x86_streambuf(p, ios)?;
        if msvc80_x86_streambuf_sputc(p, streambuf, b'\n')? as u32 == u32::MAX {
            msvc80_x86_setstate(p, ios, 0x4)?;
        }
        if p.rd32(ios + MSVC80_X86_IOS_STATE)? == 0 {
            msvc80_x86_ostream_osfx(p, ostream)?;
        }
    }
    msvc80_x86_ostream_flush(p, ostream)?;
    Ok(ostream)
}

fn msvc80_x86_mutex_lock(p: &mut Process) -> R {
    let this = msvc80_x86_this(p, "_Mutex::_Lock")?;
    let mutex = p.rd32(this)? as u64;
    if mutex == 0 {
        return Err(Stop::Crash(
            "_Mutex::_Lock found a null runtime mutex".into(),
        ));
    }
    msvc_lock_acquire(p, mutex)?;
    Ok(0)
}

fn msvc80_x86_mutex_unlock(p: &mut Process) -> R {
    let this = msvc80_x86_this(p, "_Mutex::_Unlock")?;
    let mutex = p.rd32(this)? as u64;
    if mutex == 0 {
        return Err(Stop::Crash(
            "_Mutex::_Unlock found a null runtime mutex".into(),
        ));
    }
    if !p.release_critical(mutex, p.current_thread_id) {
        return Err(msvc80_x86_unimplemented(
            p,
            format!("_Mutex::_Unlock without owning runtime mutex {mutex:#x}"),
        ));
    }
    Ok(0)
}

fn msvc80_x86_data_stream_import(p: &mut Process, name: &str) -> Result<u64, Stop> {
    let (object_key, streambuf_key, unitbuf) = match name {
        MSVC_STD_COUT_SYMBOL => (MSVC_STD_COUT_OBJECT_X86, MSVC_STD_COUT_BUFFER_X86, false),
        MSVC_STD_CERR_SYMBOL => (MSVC_STD_CERR_OBJECT_X86, MSVC_STD_CERR_BUFFER_X86, true),
        MSVC_STD_CIN_SYMBOL => (MSVC_STD_CIN_OBJECT_X86, MSVC_STD_CIN_BUFFER_X86, false),
        _ => {
            return Err(Stop::Crash(format!(
                "unknown MSVCP80 standard stream {name}"
            )))
        }
    };
    let object = data_cell(p, object_key, 0x80);
    let initialized_key = format!("{object_key}:initialized");
    if p.extra.data_cells.contains_key(&initialized_key) {
        return Ok(object);
    }
    let vbtable = data_cell(p, &format!("{object_key}:vbtable"), 8);
    p.wr32(vbtable, 0)?;
    p.wr32(vbtable + 4, 0x10)?;
    p.wr32(object, vbtable as u32)?;
    let ios = object + 0x10;
    p.wr32(ios, 0)?;
    p.wr32(ios + 4, 0)?;
    p.wr32(ios + MSVC80_X86_IOS_STATE, 0)?;
    p.wr32(ios + MSVC80_X86_IOS_EXCEPT, 0)?;
    p.wr32(
        ios + MSVC80_X86_IOS_FLAGS,
        0x201 | if unitbuf { 0x2 } else { 0 },
    )?;
    p.wr32(ios + 0x14, 6)?;
    p.wr32(ios + MSVC80_X86_IOS_WIDTH, 0)?;
    let streambuf = object + 0x40;
    p.extra.data_cells.insert(streambuf_key.into(), streambuf);
    p.wr32(ios + MSVC80_X86_IOS_STREAMBUF, streambuf as u32)?;
    p.wr8(ios + MSVC80_X86_IOS_FILL, b' ')?;
    p.wr32(streambuf, 0)?;
    p.wr32(streambuf + 0x24, 0)?;
    p.wr32(streambuf + 0x34, 0)?;
    p.extra.data_cells.insert(initialized_key, 1);
    Ok(object)
}

fn msvc_standard_stream_import(p: &mut Process, name: &str) -> Result<u64, Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        return msvc80_x86_data_stream_import(p, name);
    }
    if name == MSVC_STD_CIN_SYMBOL {
        return Err(msvc80_x86_unimplemented(
            p,
            "std::cin data object outside the PE32 model",
        ));
    }
    msvc_standard_ostream_import(p, name)
}

fn msvc_standard_ostream_import(p: &mut Process, name: &str) -> Result<u64, Stop> {
    let cout = data_cell(p, MSVC_STD_COUT_OBJECT, 0xe0);
    let cout_init_key = "msvcp:std::cout:initialized";
    if !p.extra.data_cells.contains_key(cout_init_key) {
        let streambuf = cout + 0x70;
        p.extra
            .data_cells
            .insert(MSVC_STD_COUT_BUFFER.into(), streambuf);
        msvc_standard_ostream_init(p, cout, streambuf, false, 0)?;
        p.extra.data_cells.insert(cout_init_key.into(), 1);
    }
    if name == MSVC_STD_COUT_SYMBOL {
        return Ok(cout);
    }

    let cerr = data_cell(p, MSVC_STD_CERR_OBJECT, 0xe0);
    let cerr_init_key = "msvcp:std::cerr:initialized";
    if !p.extra.data_cells.contains_key(cerr_init_key) {
        let streambuf = cerr + 0x70;
        p.extra
            .data_cells
            .insert(MSVC_STD_CERR_BUFFER.into(), streambuf);
        msvc_standard_ostream_init(p, cerr, streambuf, true, cout)?;
        p.extra.data_cells.insert(cerr_init_key.into(), 1);
    }
    Ok(cerr)
}

fn msvc_standard_ostream_flush(p: &mut Process) -> R {
    let this = p.arg(0);
    let ios = msvc_basic_ostream_ios(p, this)?;
    let state = p.rd32(ios + 0x10)?;
    if state != 0 {
        return Ok(this);
    }
    let streambuf = p.rd64(ios + 0x48)?;
    if streambuf == 0 {
        p.wr32(ios + 0x10, (state | 0x4) & 0x17)?;
        return Ok(this);
    }
    let vtable = p.rd64(streambuf)?;
    let sync = p.rd64(vtable + 13 * 8)?;
    if p.call_guest(sync, &[streambuf])? as i32 == -1 {
        let state = (state | 0x4) & 0x17;
        p.wr32(ios + 0x10, state)?;
        if state & p.rd32(ios + 0x14)? != 0 {
            return Err(Stop::Unimplemented {
                dll: "msvcp".into(),
                name: "basic_ostream<char>::flush throwing ios_base::failure".into(),
            }
            .record(p));
        }
    }
    Ok(this)
}

/// Match MSVC's `_Thrd_hardware_concurrency`: count logical processors from
/// `RelationProcessorPackage` records returned by GetLogicalProcessorInformationEx.
fn msvc_thrd_hardware_concurrency(p: &mut Process) -> R {
    const RELATION_PROCESSOR_PACKAGE: u32 = 3;
    const INITIAL_BUFFER_SIZE: u32 = 48;

    let api = p
        .thunk_for("kernel32", "GetLogicalProcessorInformationEx", true)
        .expect("creating a GetLogicalProcessorInformationEx thunk");
    let length = data_cell(p, "msvcp:_Thrd_hardware_concurrency:length", 4);
    let mut buffer_size = INITIAL_BUFFER_SIZE;
    let mut buffer = data_cell(
        p,
        "msvcp:_Thrd_hardware_concurrency:buffer",
        buffer_size as u64,
    );

    for _ in 0..4 {
        p.wr32(length, buffer_size)?;
        let success = p.call_guest(api, &[RELATION_PROCESSOR_PACKAGE as u64, buffer, length])?;
        if success != 0 {
            let bytes_returned = p.rd32(length)? as u64;
            let mut offset = 0u64;
            let mut processors = 0u32;
            while offset + 8 <= bytes_returned {
                let relationship = p.rd32(buffer + offset)?;
                let record_size = p.rd32(buffer + offset + 4)? as u64;
                if record_size < 8 || offset + record_size > bytes_returned {
                    return Ok(0);
                }
                if relationship == RELATION_PROCESSOR_PACKAGE {
                    if record_size < 32 {
                        return Ok(0);
                    }
                    let groups = p.rd16(buffer + offset + 30)? as u64;
                    if 32 + groups * 16 > record_size {
                        return Ok(0);
                    }
                    for group in 0..groups {
                        processors = processors.saturating_add(
                            p.rd64(buffer + offset + 32 + group * 16)?.count_ones(),
                        );
                    }
                }
                offset += record_size;
            }
            return Ok(processors as u64);
        }

        let required = p.rd32(length)?;
        if required <= buffer_size || required > 1 << 20 {
            return Ok(0);
        }
        buffer_size = required;
        buffer = kernel32::alloc_block(p, buffer_size as u64, true);
    }

    Ok(0)
}

/// Release the synthetic locale cell owned by an HLE-created stream buffer.
fn msvc_basic_streambuf_char_destructor(p: &mut Process) -> R {
    p.wr64(p.arg(0) + 0x60, 0)?;
    Ok(0)
}

fn msvc_basic_streambuf_wchar_destructor(p: &mut Process) -> R {
    p.wr64(p.arg(0) + 0x60, 0)?;
    Ok(0)
}

/// MSVC declares the `basic_iostream<char>` destructor with an empty body.
fn msvc_basic_iostream_char_destructor(_p: &mut Process) -> R {
    Ok(0)
}

fn msvc_basic_iostream_wchar_destructor(_p: &mut Process) -> R {
    Ok(0)
}

/// MSVC declares the `basic_ios<char>` destructor with an empty body.
fn msvc_basic_ios_char_destructor(_p: &mut Process) -> R {
    Ok(0)
}

fn msvc_basic_ostream_ios(p: &mut Process, this: u64) -> R {
    let vbtable = p.rd64(this)?;
    let ios_offset = p.rd32(vbtable + 4)? as i32;
    if ios_offset >= 0 {
        this.checked_add(ios_offset as u64)
    } else {
        this.checked_sub(ios_offset.unsigned_abs() as u64)
    }
    .ok_or_else(|| Stop::Crash("basic_ostream virtual-base address overflow".into()))
}

/// `basic_ostream::_Osfx()` synchronizes only a good stream with `unitbuf` set.
fn msvc_basic_ostream_char_osfx(p: &mut Process) -> R {
    let this = p.arg(0);
    let ios = msvc_basic_ostream_ios(p, this)?;

    let state = p.rd32(ios + 0x10)?;
    let flags = p.rd32(ios + 0x18)?;
    if state != 0 || flags & 0x2 == 0 {
        return Ok(0);
    }

    let streambuf = p.rd64(ios + 0x48)?;
    let vtable = p.rd64(streambuf)?;
    let sync = p.rd64(vtable + 13 * 8)?;
    if p.call_guest(sync, &[streambuf])? as i32 == -1 {
        // `_Osfx()` is noexcept and catches failures from setting badbit.
        p.wr32(ios + 0x10, (state | 0x4) & 0x17)?;
    }
    Ok(0)
}

fn msvc_ostream_sputn(p: &mut Process, streambuf: u64, bytes: &[u8]) -> R {
    if bytes.is_empty() {
        return Ok(0);
    }
    let scratch = data_cell(p, "msvcp:ostream:format-scratch", 0x100);
    p.write_bytes(scratch, bytes)?;
    let vtable = p.rd64(streambuf)?;
    let sputn = p.rd64(vtable + 9 * 8)?;
    p.call_guest(sputn, &[streambuf, scratch, bytes.len() as u64])
}

fn msvc_ostream_write_padding(p: &mut Process, streambuf: u64, mut count: u64, fill: u8) -> R {
    let chunk = [fill; 128];
    while count > 0 {
        let size = count.min(chunk.len() as u64) as usize;
        if msvc_ostream_sputn(p, streambuf, &chunk[..size])? != size as u64 {
            return Ok(0);
        }
        count -= size as u64;
    }
    Ok(1)
}

/// Insert an integer using the stream's base, adjustment, fill, and width fields. The target in
/// this path uses the default C locale, whose integer punctuation is empty.
fn msvc_ostream_insert_integer(
    p: &mut Process,
    mut value: u64,
    negative: bool,
    operand_bits: u32,
) -> R {
    let this = p.arg(0);
    let ios = msvc_basic_ostream_ios(p, this)?;
    let streambuf = p.rd64(ios + 0x48)?;

    if streambuf != 0 {
        let vtable = p.rd64(streambuf)?;
        let lock = p.rd64(vtable + 8)?;
        p.call_guest(lock, &[streambuf])?;
    }

    let state = p.rd32(ios + 0x10)?;
    if state != 0 || streambuf == 0 {
        if streambuf != 0 {
            let vtable = p.rd64(streambuf)?;
            let unlock = p.rd64(vtable + 16)?;
            p.call_guest(unlock, &[streambuf])?;
        }
        return Ok(this);
    }

    let flags = p.rd32(ios + 0x18)?;
    let basefield = flags & 0x0e00;
    let (base, uppercase) = match basefield {
        0x0400 => (8, false),
        0x0800 => (16, flags & 0x4 != 0),
        _ => (10, false),
    };
    if negative && base != 10 {
        let operand_mask = if operand_bits == 64 {
            u64::MAX
        } else {
            (1u64 << operand_bits) - 1
        };
        value = 0u64.wrapping_sub(value) & operand_mask;
    }
    let mut digits = [0u8; 64];
    let mut start = digits.len();
    let alphabet = if uppercase {
        b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
    } else {
        b"0123456789abcdefghijklmnopqrstuvwxyz"
    };
    let mut remaining = value;
    loop {
        start -= 1;
        digits[start] = alphabet[(remaining % base) as usize];
        remaining /= base;
        if remaining == 0 {
            break;
        }
    }

    let mut prefix = [0u8; 2];
    let mut prefix_len = 0;
    if flags & 0x8 != 0 {
        if base == 16 && value != 0 {
            prefix[..2].copy_from_slice(if uppercase { b"0X" } else { b"0x" });
            prefix_len = 2;
        } else if base == 8 && digits[start] != b'0' {
            prefix[0] = b'0';
            prefix_len = 1;
        }
    }
    let sign = if negative && base == 10 {
        Some(b'-')
    } else if flags & 0x20 != 0 && !negative {
        Some(b'+')
    } else {
        None
    };
    let mut body = [0u8; 67];
    let mut body_len = 0;
    if let Some(sign) = sign {
        body[body_len] = sign;
        body_len += 1;
    }
    body[body_len..body_len + prefix_len].copy_from_slice(&prefix[..prefix_len]);
    body_len += prefix_len;
    body[body_len..body_len + digits.len() - start].copy_from_slice(&digits[start..]);
    body_len += digits.len() - start;

    let width = p.rd64(ios + 0x28)? as i64;
    p.wr64(ios + 0x28, 0)?;
    let padding = (width - body_len as i64).max(0) as u64;
    if padding > 1_048_576 {
        return Err(Stop::Unimplemented {
            dll: "msvcp".into(),
            name: "basic_ostream integer width above 1048576".into(),
        }
        .record(p));
    }
    let fill = p.rd8(ios + 0x58)?;
    let left = flags & 0x40 != 0;
    let internal = !left && flags & 0x100 != 0;
    let mut written = true;
    if !left && !internal && padding != 0 {
        written = msvc_ostream_write_padding(p, streambuf, padding, fill)? != 0;
    }
    if written && internal && padding != 0 {
        let prefix_end = usize::from(sign.is_some()) + prefix_len;
        written = msvc_ostream_sputn(p, streambuf, &body[..prefix_end])? == prefix_end as u64;
        if written {
            written = msvc_ostream_write_padding(p, streambuf, padding, fill)? != 0;
        }
        if written {
            written = msvc_ostream_sputn(p, streambuf, &body[prefix_end..body_len])?
                == (body_len - prefix_end) as u64;
        }
    } else if written {
        written = msvc_ostream_sputn(p, streambuf, &body[..body_len])? == body_len as u64;
    }
    if written && left && padding != 0 {
        written = msvc_ostream_write_padding(p, streambuf, padding, fill)? != 0;
    }

    if !written {
        let state = (p.rd32(ios + 0x10)? | 0x4) & 0x17;
        p.wr32(ios + 0x10, state)?;
        if state & p.rd32(ios + 0x14)? != 0 {
            let vtable = p.rd64(streambuf)?;
            let unlock = p.rd64(vtable + 16)?;
            p.call_guest(unlock, &[streambuf])?;
            return Err(Stop::Unimplemented {
                dll: "msvcp".into(),
                name: "basic_ostream integer insertion throwing ios_base::failure".into(),
            }
            .record(p));
        }
    }

    if p.rd32(ios + 0x10)? == 0 && p.rd32(ios + 0x18)? & 0x2 != 0 {
        msvc_basic_ostream_char_osfx(p)?;
    }
    let vtable = p.rd64(streambuf)?;
    let unlock = p.rd64(vtable + 16)?;
    p.call_guest(unlock, &[streambuf])?;
    Ok(this)
}

fn msvc_ostream_insert_i32(p: &mut Process) -> R {
    let value = p.arg(1) as u32 as i32;
    msvc_ostream_insert_integer(p, value.unsigned_abs() as u64, value < 0, 32)
}

fn msvc_ostream_insert_u16(p: &mut Process) -> R {
    let value = p.arg(1) as u16 as u64;
    msvc_ostream_insert_integer(p, value, false, 16)
}

fn msvc_ostream_insert_u32(p: &mut Process) -> R {
    let value = p.arg(1) as u32 as u64;
    msvc_ostream_insert_integer(p, value, false, 32)
}

fn msvc_ostream_insert_u64(p: &mut Process) -> R {
    let value = p.arg(1);
    msvc_ostream_insert_integer(p, value, false, 64)
}

fn msvc_thrd_yield(p: &mut Process) -> R {
    p.request_thread_yield();
    Ok(0)
}

fn msvc_ostream_insert_ios_base_manipulator(p: &mut Process) -> R {
    let this = p.arg(0);
    let manipulator = p.arg(1);
    let ios = msvc_basic_ostream_ios(p, this)?;
    p.call_guest(manipulator, &[ios])?;
    Ok(this)
}

fn msvc_ostream_insert_float(p: &mut Process) -> R {
    // `this` occupies RCX, so the float parameter in the second Win64 slot is XMM1.
    let value = f32::from_bits(p.cpu.xmm[1] as u32) as f64;
    msvc_ostream_insert_floating(p, value)
}

fn msvc_ostream_insert_double(p: &mut Process) -> R {
    let value = f64::from_bits(p.cpu.xmm[1] as u64);
    msvc_ostream_insert_floating(p, value)
}

fn msvc_ostream_insert_floating(p: &mut Process, value: f64) -> R {
    let this = p.arg(0);
    let ios = msvc_basic_ostream_ios(p, this)?;
    let streambuf = p.rd64(ios + 0x48)?;

    if streambuf != 0 {
        let vtable = p.rd64(streambuf)?;
        let lock = p.rd64(vtable + 8)?;
        p.call_guest(lock, &[streambuf])?;
    }

    let state = p.rd32(ios + 0x10)?;
    if state != 0 || streambuf == 0 {
        if streambuf != 0 {
            let vtable = p.rd64(streambuf)?;
            let unlock = p.rd64(vtable + 16)?;
            p.call_guest(unlock, &[streambuf])?;
        }
        return Ok(this);
    }

    let flags = p.rd32(ios + 0x18)?;
    let precision = (p.rd64(ios + 0x20)? as i64).clamp(0, 128) as usize;
    let output = printf::format_ostream_float(value, precision, flags);
    let bytes = output.as_bytes();
    let width = p.rd64(ios + 0x28)? as i64;
    p.wr64(ios + 0x28, 0)?;
    let padding = (width - bytes.len() as i64).max(0) as u64;
    if padding > 1_048_576 {
        let vtable = p.rd64(streambuf)?;
        let unlock = p.rd64(vtable + 16)?;
        p.call_guest(unlock, &[streambuf])?;
        return Err(Stop::Unimplemented {
            dll: "msvcp".into(),
            name: "basic_ostream float width above 1048576".into(),
        }
        .record(p));
    }

    let fill = p.rd8(ios + 0x58)?;
    let left = flags & 0x40 != 0;
    let internal = !left && flags & 0x100 != 0;
    let prefix_len = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut written = true;
    if !left && !internal && padding != 0 {
        written = msvc_ostream_write_padding(p, streambuf, padding, fill)? != 0;
    }
    if written && internal && padding != 0 {
        written = msvc_ostream_sputn(p, streambuf, &bytes[..prefix_len])? == prefix_len as u64;
        if written {
            written = msvc_ostream_write_padding(p, streambuf, padding, fill)? != 0;
        }
        if written {
            written = msvc_ostream_sputn(p, streambuf, &bytes[prefix_len..])?
                == (bytes.len() - prefix_len) as u64;
        }
    } else if written {
        written = msvc_ostream_sputn(p, streambuf, bytes)? == bytes.len() as u64;
    }
    if written && left && padding != 0 {
        written = msvc_ostream_write_padding(p, streambuf, padding, fill)? != 0;
    }

    if !written {
        let state = (p.rd32(ios + 0x10)? | 0x4) & 0x17;
        p.wr32(ios + 0x10, state)?;
        if state & p.rd32(ios + 0x14)? != 0 {
            let vtable = p.rd64(streambuf)?;
            let unlock = p.rd64(vtable + 16)?;
            p.call_guest(unlock, &[streambuf])?;
            return Err(Stop::Unimplemented {
                dll: "msvcp".into(),
                name: "basic_ostream float insertion throwing ios_base::failure".into(),
            }
            .record(p));
        }
    }

    if p.rd32(ios + 0x10)? == 0 && p.rd32(ios + 0x18)? & 0x2 != 0 {
        msvc_basic_ostream_char_osfx(p)?;
    }
    let vtable = p.rd64(streambuf)?;
    let unlock = p.rd64(vtable + 16)?;
    p.call_guest(unlock, &[streambuf])?;
    Ok(this)
}

/// `ios_base::good()` is exactly a zero check of `_Mystate` in this MSVC layout.
fn msvc_ios_base_good(p: &mut Process) -> R {
    Ok(u64::from(p.rd32(p.arg(0) + 0x10)? == 0))
}

fn msvc_ios_base_set_precision(p: &mut Process) -> R {
    p.wr64(p.arg(0) + 0x20, p.arg(1))?;
    Ok(0)
}

fn msvc_ios_base_set_width(p: &mut Process) -> R {
    p.wr64(p.arg(0) + 0x28, p.arg(1))?;
    Ok(0)
}

fn msvc_setprecision(p: &mut Process) -> R {
    let output = p.arg(0);
    let precision_setter = p
        .thunk_for("msvcp", "__anywasm_ios_base_set_precision", true)
        .expect("creating the MSVC ios_base precision setter thunk");
    p.wr64(output, precision_setter)?;
    p.wr64(output + 8, p.arg(1))?;
    Ok(output)
}

fn msvc_setw(p: &mut Process) -> R {
    let output = p.arg(0);
    let width_setter = p
        .thunk_for("msvcp", "__anywasm_ios_base_set_width", true)
        .expect("creating the MSVC ios_base width setter thunk");
    p.wr64(output, width_setter)?;
    p.wr64(output + 8, p.arg(1))?;
    Ok(output)
}

/// Return the one-pointer `std::locale` value through the MSVC x64 hidden
/// structure-return pointer. Empty synthetic locales carry a null `_Locimp`;
/// their destructor is consequently a no-op.
fn msvc_ios_base_getloc(p: &mut Process) -> R {
    // For this MSVC member function, RCX carries `this` and RDX carries the
    // hidden return buffer for the non-trivial locale value.
    let this = p.arg(0);
    let output = p.arg(1);
    let locale = p.rd64(this + 0x40)?;
    let implementation = if locale == 0 { 0 } else { p.rd64(locale + 8)? };
    if implementation != 0 {
        let refs = p.rd32(implementation + 8)?;
        p.wr32(implementation + 8, refs.saturating_add(1))?;
    }
    p.wr64(output, 0)?; // empty _Locbase<int> base
    p.wr64(output + 8, implementation)?;
    Ok(output)
}

/// Return the stable process-global C locale implementation used by synthesized stream values.
fn msvc_get_global_locale(p: &mut Process) -> R {
    let locale = msvc_locale_value(p, "msvcp:std::locale:global")?;
    Ok(p.rd64(locale + 8)?)
}

fn msvc_ctype_wchar_getcat(p: &mut Process) -> R {
    let facet_out = p.arg(0);
    if facet_out != 0 {
        if p.rd64(facet_out)? == 0 {
            let facet = data_cell(p, "msvcp:ctype:wchar:facet", 0x40);
            msvc_locale_facet_init(p, facet, 0)?;
            p.wr64(facet_out, facet)?;
        }
    }
    // MSVC's _X_CTYPE category mask.
    Ok(1)
}

fn msvc_ctype_wchar_widen(p: &mut Process) -> R {
    // The target path widens ASCII delimiters. The C locale maps these
    // one-byte characters directly to their UTF-16 code units.
    Ok((p.arg(1) as u8) as u64)
}

const MSVC_LOCKIT_KEY_BASE: u64 = 0x7fff_ff00_0000_0000;

fn msvc_locale_facet_incref(p: &mut Process) -> R {
    let facet = p.arg(0);
    let refs = p.rd32(facet + 8)?;
    p.wr32(facet + 8, refs.saturating_add(1))?;
    Ok(0)
}

fn msvc_locale_facet_decref(p: &mut Process) -> R {
    let facet = p.arg(0);
    let refs = p.rd32(facet + 8)?;
    let refs = refs.saturating_sub(1);
    p.wr32(facet + 8, refs)?;
    Ok(if refs == 0 { facet } else { 0 })
}

fn msvc_locale_facet_destructor(_p: &mut Process) -> R {
    Ok(0)
}

fn msvc_locale_facet_init(p: &mut Process, facet: u64, initial_refs: u32) -> Result<(), Stop> {
    let vtable = data_cell(p, "msvcp:locale:facet:vtable", 0x40);
    if p.rd64(vtable)? == 0 {
        let unsupported = p
            .thunk_for("msvcp", "__anywasm_unimplemented_std_locale_virtual", true)
            .expect("creating an explicit locale virtual-call trap");
        for offset in (0..0x40).step_by(8) {
            p.wr64(vtable + offset, unsupported)?;
        }
        for (offset, name) in [
            (0, "__anywasm_locale_facet_destructor"),
            (8, "__anywasm_locale_facet_incref"),
            (16, "__anywasm_locale_facet_decref"),
        ] {
            let target = p
                .thunk_for("msvcp", name, true)
                .expect("creating a locale facet thunk");
            p.wr64(vtable + offset, target)?;
        }
    }
    p.wr64(facet, vtable)?;
    p.wr32(facet + 8, initial_refs)?;
    Ok(())
}

/// A stable synthetic locale implementation with an empty facet vector.
fn msvc_locale_value(p: &mut Process, key: &str) -> Result<u64, Stop> {
    let implementation = data_cell(p, "msvcp:std::locale:implementation", 0x60);
    if p.rd64(implementation)? == 0 {
        msvc_locale_facet_init(p, implementation, 1)?;
        p.wr64(implementation + 0x10, 0)?; // _Facetvec
        p.wr64(implementation + 0x18, 0)?; // _Facetcount
        p.wr32(implementation + 0x20, 0)?; // _Catmask
        p.wr8(implementation + 0x24, 0)?; // _Xparent
    }
    let locale = data_cell(p, key, 16);
    p.wr64(locale, 0)?; // empty _Locbase<int> base
    p.wr64(locale + 8, implementation)?;
    Ok(locale)
}

fn msvc_lock_acquire(p: &mut Process, address: u64) -> Result<(), Stop> {
    let id = p.current_thread_id;
    if p.try_acquire_critical(address, id) {
        return Ok(());
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "msvcp".into(),
            name: "contended CRT lock from a nested synchronous guest callback".into(),
        });
    }
    let waiters = &mut p.critical_sections.entry(address).or_default().waiters;
    if !waiters.contains(&id) {
        waiters.push_back(id);
    }
    p.block_current_thread(ThreadWait::CriticalAcquire { address });
    Ok(())
}

fn msvc_lockit_ctor(p: &mut Process) -> R {
    let this = p.arg(0);
    let lock_no = p.arg(1) as i32;
    p.wr32(this, lock_no as u32)?;
    if lock_no >= 0 {
        msvc_lock_acquire(p, MSVC_LOCKIT_KEY_BASE + lock_no as u64)?;
    }
    Ok(this)
}

fn msvc_lockit_dtor(p: &mut Process) -> R {
    let lock_no = p.rd32(p.arg(0))? as i32;
    if lock_no >= 0 {
        p.release_critical(MSVC_LOCKIT_KEY_BASE + lock_no as u64, p.current_thread_id);
    }
    Ok(0)
}

fn msvc_mtx_lock(p: &mut Process) -> R {
    msvc_lock_acquire(p, p.arg(0))?;
    Ok(0)
}

fn msvc_mtx_unlock(p: &mut Process) -> R {
    p.release_critical(p.arg(0), p.current_thread_id);
    Ok(0)
}

fn msvc_cnd_init(_p: &mut Process) -> R {
    Ok(0)
}

fn msvc_cnd_destroy(_p: &mut Process) -> R {
    Ok(0)
}

fn msvc_cnd_signal(p: &mut Process) -> R {
    p.wake_condition(p.arg(0), false);
    Ok(0)
}

fn msvc_cnd_broadcast(p: &mut Process) -> R {
    p.wake_condition(p.arg(0), true);
    Ok(0)
}

fn msvc_cnd_wait(p: &mut Process) -> R {
    let condition = p.arg(0);
    let lock = p.arg(1);
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "msvcp".into(),
            name: "blocking _Cnd_wait from a nested synchronous guest callback".into(),
        });
    }
    let owned_once = p
        .critical_sections
        .get(&lock)
        .is_some_and(|state| state.owner == Some(p.current_thread_id) && state.recursion == 1);
    if !owned_once || !p.release_critical(lock, p.current_thread_id) {
        return Ok(4); // _Thrd_error
    }
    p.block_current_thread(ThreadWait::Condition {
        condition,
        lock,
        critical: true,
        exclusive: true,
        deadline: None,
        outcome: None,
    });
    Ok(0) // _Thrd_success
}

/// Merge a new iostream state into `_Mystate`, preserving MSVC's `_Statmask`.
/// Throwing `ios_base::failure` remains explicit until C++ exceptions are wired here.
fn msvc_basic_ios_char_setstate(p: &mut Process) -> R {
    let this = p.arg(0);
    let state = (p.rd32(this + 0x10)? | p.arg(1) as u32) & 0x17;
    p.wr32(this + 0x10, state)?;
    if state & p.rd32(this + 0x14)? != 0 {
        return Err(Stop::Unimplemented {
            dll: "msvcp".into(),
            name: "basic_ios<char>::setstate throwing ios_base::failure".into(),
        }
        .record(p));
    }
    Ok(0)
}

fn msvc_uncaught_exception(p: &mut Process) -> R {
    Ok(u64::from(
        p.guest_exception_depths
            .get(&p.current_thread_id)
            .copied()
            .unwrap_or(0)
            != 0,
    ))
}

/// Initialize the `basic_iostream` object layout observed from MSVCP140's
/// narrow and wide constructors. Locale internals stay guest-owned opaque cells;
/// virtual calls trap explicitly until their individual slots have implementations.
fn msvc_basic_iostream_default(p: &mut Process, char_type: &str) -> R {
    let this = p.arg(0);
    let streambuf = p.arg(1);

    // Base constructors can run inside a larger stream object. Keep its vbtable
    // and use the encoded virtual-base displacement; standalone calls use +0x20.
    let ios_offset = p
        .rd64(this)
        .ok()
        .and_then(|table| p.rd32(table + 4).ok())
        .map(|offset| offset as i32)
        .and_then(|offset| {
            let address = if offset >= 0 {
                this.checked_add(offset as u64)?
            } else {
                this.checked_sub(offset.unsigned_abs() as u64)?
            };
            p.rd64(address).ok().map(|_| offset)
        });
    let ios_offset = if let Some(offset) = ios_offset {
        offset
    } else {
        let vbtable_a = data_cell(
            p,
            &format!("msvcp:basic_iostream:{char_type}:vbtable-a"),
            12,
        );
        p.wr32(vbtable_a, 0)?;
        p.wr32(vbtable_a + 4, 32)?;
        p.wr32(vbtable_a + 8, 0)?;
        p.wr64(this, vbtable_a)?;

        let vbtable_b = data_cell(
            p,
            &format!("msvcp:basic_iostream:{char_type}:vbtable-b"),
            12,
        );
        p.wr32(vbtable_b, 0)?;
        p.wr32(vbtable_b + 4, 16)?;
        p.wr32(vbtable_b + 8, 0)?;
        p.wr64(this + 0x10, vbtable_b)?;
        32
    };

    p.wr64(this + 8, 0)?;
    // The native constructor leaves this dword untouched.
    p.wr32(this + 0x1c, 0)?;

    let vtable = data_cell(p, &format!("msvcp:basic_iostream:{char_type}:vtable"), 0x80);
    let unsupported = p
        .thunk_for(
            "msvcp",
            &format!("__anywasm_unimplemented_basic_iostream_{char_type}_virtual"),
            true,
        )
        .expect("creating an explicit MSVCP virtual-call trap");
    for offset in (0..0x80).step_by(8) {
        p.wr64(vtable + offset, unsupported)?;
    }

    let locale = msvc_locale_value(p, &format!("msvcp:basic_iostream:{char_type}:locale"))?;
    let ios = (this as i64 + ios_offset as i64) as u64;
    p.wr64(ios, vtable)?;
    p.wr64(ios + 8, 0)?;
    p.wr32(ios + 0x10, if streambuf == 0 { 4 } else { 0 })?;
    p.wr32(ios + 0x14, 0)?;
    p.wr32(ios + 0x18, 0x201)?;
    p.wr64(ios + 0x20, 6)?;
    for offset in [0x28, 0x30, 0x38] {
        p.wr64(ios + offset, 0)?;
    }
    p.wr64(ios + 0x40, locale)?;
    p.wr64(ios + 0x48, streambuf)?;
    p.wr64(ios + 0x50, 0)?;
    if char_type == "wchar" {
        p.wr16(ios + 0x58, 0x20)?;
    } else {
        p.wr8(ios + 0x58, 0x20)?;
    }

    Ok(this)
}

fn msvc_basic_iostream_char_default(p: &mut Process) -> R {
    msvc_basic_iostream_default(p, "char")
}

fn msvc_basic_iostream_wchar_default(p: &mut Process) -> R {
    msvc_basic_iostream_default(p, "wchar")
}

/// Initialize the `basic_streambuf<char>` default layout observed from MSVCP140.
/// The native locale pointer is replaced with guest-owned opaque storage.
fn msvc_basic_streambuf_default(p: &mut Process, char_type: &str, xsputn_name: &str) -> R {
    let this = p.arg(0);
    let vtable = data_cell(
        p,
        &format!("msvcp:basic_streambuf:{char_type}:vtable"),
        0x80,
    );
    let unsupported = p
        .thunk_for(
            "msvcp",
            &format!("__anywasm_unimplemented_basic_streambuf_{char_type}_virtual"),
            true,
        )
        .expect("creating an explicit MSVCP virtual-call trap");
    for offset in (0..0x80).step_by(8) {
        p.wr64(vtable + offset, unsupported)?;
    }
    let xsputn = p
        .thunk_for("msvcp", xsputn_name, true)
        .expect("creating the MSVCP basic_streambuf xsputn thunk");
    p.wr64(vtable + 9 * 8, xsputn)?;
    let (underflow_name, uflow_name, xsgetn_name) = if char_type == "wchar" {
        (
            "__anywasm_basic_streambuf_wchar_underflow",
            "__anywasm_basic_streambuf_wchar_uflow",
            "__anywasm_basic_streambuf_wchar_xsgetn",
        )
    } else {
        (
            "__anywasm_basic_streambuf_char_underflow",
            "__anywasm_basic_streambuf_char_uflow",
            "__anywasm_basic_streambuf_char_xsgetn",
        )
    };
    for (slot, name) in [(6, underflow_name), (7, uflow_name), (8, xsgetn_name)] {
        let target = p
            .thunk_for("msvcp", name, true)
            .expect("creating an MSVCP basic_streambuf input thunk");
        p.wr64(vtable + slot * 8, target)?;
    }
    let sync = p
        .thunk_for("msvcp", "__anywasm_basic_streambuf_char_sync", true)
        .expect("creating the MSVCP basic_streambuf sync thunk");
    p.wr64(vtable + 13 * 8, sync)?;
    let lock_noop = p
        .thunk_for("msvcp", "__anywasm_basic_streambuf_char_lock_noop", true)
        .expect("creating the MSVCP basic_streambuf lock no-op thunk");
    p.wr64(vtable + 8, lock_noop)?;
    p.wr64(vtable + 16, lock_noop)?;
    let (showmanyc_name, setbuf_name, imbue_name) = if char_type == "wchar" {
        (
            "?showmanyc@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAA_JXZ",
            "?setbuf@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAAPEAV12@PEA_W_J@Z",
            "?imbue@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAAXAEBVlocale@2@@Z",
        )
    } else {
        (
            "?showmanyc@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAA_JXZ",
            "?setbuf@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAAPEAV12@PEAD_J@Z",
            "?imbue@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAAXAEBVlocale@2@@Z",
        )
    };
    for (offset, name) in [
        (5 * 8, showmanyc_name),
        (12 * 8, setbuf_name),
        (14 * 8, imbue_name),
    ] {
        let target = p
            .thunk_for("msvcp", name, true)
            .expect("creating an MSVCP basic_streambuf default-virtual thunk");
        p.wr64(vtable + offset, target)?;
    }

    let locale = msvc_locale_value(p, &format!("msvcp:basic_streambuf:{char_type}:locale"))?;
    p.wr64(this, vtable)?;
    p.wr64(this + 0x08, 0)?;
    p.wr64(this + 0x10, 0)?;
    p.wr64(this + 0x18, this + 0x08)?;
    p.wr64(this + 0x20, this + 0x10)?;
    p.wr64(this + 0x28, 0)?;
    p.wr64(this + 0x30, 0)?;
    p.wr64(this + 0x38, this + 0x28)?;
    p.wr64(this + 0x40, this + 0x30)?;
    p.wr64(this + 0x48, 0)?;
    p.wr64(this + 0x50, this + 0x48)?;
    p.wr64(this + 0x58, this + 0x4c)?;
    p.wr64(this + 0x60, locale)?;
    Ok(this)
}

fn msvc_basic_streambuf_char_default(p: &mut Process) -> R {
    msvc_basic_streambuf_default(p, "char", "__anywasm_basic_streambuf_char_xsputn")
}

fn msvc_basic_streambuf_wchar_default(p: &mut Process) -> R {
    msvc_basic_streambuf_default(p, "wchar", "__anywasm_basic_streambuf_wchar_xsputn")
}

const CRT_WIDE_STRING_MAX_UNITS: u64 = 1 << 20;

fn crt_wide_unit_address(base: u64, index: u64) -> Result<u64, Stop> {
    index
        .checked_mul(2)
        .and_then(|offset| base.checked_add(offset))
        .ok_or_else(|| Stop::Crash(format!("wide CRT string address overflow at {base:#x}")))
}

fn crt_invalid_parameter(p: &mut Process, function: &str) -> Result<(), Stop> {
    let handler = p.extra.invalid_parameter_handler;
    if handler == 0 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("{function} default invalid-parameter Watson"),
        }
        .record(p));
    }

    // The retail CRT passes NULL metadata; the debug CRT fills these fields with source details.
    p.call_guest(handler, &[0; 5])?;
    Ok(())
}

fn read_crt_wide_units(p: &Process, address: u64) -> Result<Vec<u16>, Stop> {
    let mut units = Vec::new();
    for index in 0..CRT_WIDE_STRING_MAX_UNITS {
        let offset = crt_wide_unit_address(address, index)?;
        let unit = p.rd16(offset)?;
        if unit == 0 {
            return Ok(units);
        }
        units.push(unit);
    }
    Err(Stop::Crash(format!(
        "unterminated wide CRT string at {address:#x} exceeds {CRT_WIDE_STRING_MAX_UNITS} UTF-16 code units"
    )))
}

fn crt_wcsncpy(p: &mut Process) -> R {
    let (destination, source, count) = (p.arg(0), p.arg(1), p.arg(2));
    if destination == 0 || source == 0 || count == 0 {
        crt_invalid_parameter(p, "wcsncpy")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(u32::MAX as u64);
    }
    if count > CRT_WIDE_STRING_MAX_UNITS {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("wcsncpy count {count} exceeds the translated code-unit limit"),
        }
        .record(p));
    }

    let mut source_ended = false;
    for index in 0..count {
        let unit = if source_ended {
            0
        } else {
            let unit = p.rd16(crt_wide_unit_address(source, index)?)?;
            source_ended = unit == 0;
            unit
        };
        p.wr16(crt_wide_unit_address(destination, index)?, unit)?;
    }
    Ok(destination)
}

fn crt_wcslwr(p: &mut Process) -> R {
    let string = p.arg(0);
    if string == 0 {
        crt_invalid_parameter(p, "_wcslwr")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }

    for index in 0..CRT_WIDE_STRING_MAX_UNITS {
        let address = crt_wide_unit_address(string, index)?;
        let unit = p.rd16(address)?;
        if unit == 0 {
            return Ok(string);
        }
        let lower = crt_c_locale_wide_lower(unit);
        if lower != unit {
            p.wr16(address, lower)?;
        }
    }

    Err(Stop::Crash(format!(
        "unterminated _wcslwr input exceeds {CRT_WIDE_STRING_MAX_UNITS} UTF-16 code units"
    )))
}

fn crt_wcsicmp(p: &mut Process) -> R {
    let (left, right) = (p.arg(0), p.arg(1));
    if left == 0 || right == 0 {
        crt_invalid_parameter(p, "_wcsicmp")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(i32::MAX as u32 as u64); // _NLSCMPERROR
    }

    for index in 0..CRT_WIDE_STRING_MAX_UNITS {
        let left_address = crt_wide_unit_address(left, index)?;
        let right_address = crt_wide_unit_address(right, index)?;
        let left_unit = crt_c_locale_wide_lower(p.rd16(left_address)?);
        let right_unit = crt_c_locale_wide_lower(p.rd16(right_address)?);
        if left_unit != right_unit {
            return Ok(left_unit.cmp(&right_unit) as i32 as i64 as u64);
        }
        if left_unit == 0 {
            return Ok(0);
        }
    }

    Err(Stop::Crash(format!(
        "unterminated _wcsicmp input exceeds {CRT_WIDE_STRING_MAX_UNITS} UTF-16 code units"
    )))
}

fn crt_wcsnicmp(p: &mut Process) -> R {
    let (left, right, count) = (p.arg(0), p.arg(1), p.arg(2));
    if left == 0 || right == 0 {
        crt_invalid_parameter(p, "_wcsnicmp")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(i32::MAX as u32 as u64); // _NLSCMPERROR
    }
    if count > CRT_WIDE_STRING_MAX_UNITS {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("_wcsnicmp count {count} exceeds the translated code-unit limit"),
        }
        .record(p));
    }

    for index in 0..count {
        let left_unit = crt_c_locale_wide_lower(p.rd16(crt_wide_unit_address(left, index)?)?);
        let right_unit = crt_c_locale_wide_lower(p.rd16(crt_wide_unit_address(right, index)?)?);
        if left_unit != right_unit {
            return Ok(left_unit.cmp(&right_unit) as i32 as i64 as u64);
        }
        if left_unit == 0 {
            return Ok(0);
        }
    }
    Ok(0)
}

fn crt_c_locale_wide_lower(unit: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&unit) {
        unit + (b'a' - b'A') as u16
    } else {
        unit
    }
}

fn crt_wcsstr(p: &mut Process) -> R {
    let haystack = p.arg(0);
    let needle = read_crt_wide_units(p, p.arg(1))?;
    if needle.is_empty() {
        if haystack == 0 {
            return Err(Stop::Crash(
                "wcsstr received a null haystack pointer".into(),
            ));
        }
        return Ok(haystack);
    }

    // KMP avoids quadratic work for repetitive near-matches while retaining the bounded
    // one-pass read of guest memory and exact UTF-16 code-unit matching.
    let mut prefix = vec![0usize; needle.len()];
    for index in 1..needle.len() {
        let mut matched = prefix[index - 1];
        while matched > 0 && needle[index] != needle[matched] {
            matched = prefix[matched - 1];
        }
        if needle[index] == needle[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }

    let mut matched = 0usize;
    for index in 0..CRT_WIDE_STRING_MAX_UNITS {
        let unit = p.rd16(crt_wide_unit_address(haystack, index)?)?;
        if unit == 0 {
            return Ok(0);
        }
        while matched > 0 && unit != needle[matched] {
            matched = prefix[matched - 1];
        }
        if unit == needle[matched] {
            matched += 1;
            if matched == needle.len() {
                let index = index + 1 - needle.len() as u64;
                let offset = index
                    .checked_mul(2)
                    .and_then(|offset| haystack.checked_add(offset))
                    .ok_or_else(|| {
                        Stop::Crash(format!("wcsstr result address overflow from {haystack:#x}"))
                    })?;
                return Ok(offset);
            }
        }
    }
    Err(Stop::Crash(format!(
        "unterminated wcsstr haystack at {haystack:#x} exceeds {CRT_WIDE_STRING_MAX_UNITS} UTF-16 code units"
    )))
}

fn crt_wcschr(p: &mut Process) -> R {
    let (string, character) = (p.arg(0), p.arg(1) as u16);
    if string == 0 {
        return Err(Stop::Crash("wcschr received a null string pointer".into()));
    }

    for index in 0..CRT_WIDE_STRING_MAX_UNITS {
        let address = crt_wide_unit_address(string, index)?;
        let unit = p.rd16(address)?;
        if unit == character {
            return Ok(address);
        }
        if unit == 0 {
            return Ok(0);
        }
    }

    Err(Stop::Crash(format!(
        "unterminated wcschr string at {string:#x} exceeds {CRT_WIDE_STRING_MAX_UNITS} UTF-16 code units"
    )))
}

fn crt_wcspbrk(p: &mut Process) -> R {
    let (string, set_address) = (p.arg(0), p.arg(1));
    let set_units = read_crt_wide_units(p, set_address)?;
    let set = std::collections::HashSet::<u16>::from_iter(set_units);

    for index in 0..CRT_WIDE_STRING_MAX_UNITS {
        let address = crt_wide_unit_address(string, index)?;
        let unit = p.rd16(address)?;
        if unit == 0 {
            return Ok(0);
        }
        if set.contains(&unit) {
            return Ok(address);
        }
    }

    Err(Stop::Crash(format!(
        "unterminated wcspbrk string at {string:#x} exceeds {CRT_WIDE_STRING_MAX_UNITS} UTF-16 code units"
    )))
}

fn crt_wcsncmp(p: &mut Process) -> R {
    let (left, right, count) = (p.arg(0), p.arg(1), p.arg(2));
    if count == 0 {
        return Ok(0);
    }
    if left == 0 || right == 0 {
        crt_invalid_parameter(p, "wcsncmp")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(i32::MAX as u32 as u64);
    }
    if count > CRT_WIDE_STRING_MAX_UNITS {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("wcsncmp count {count} exceeds the translated code-unit limit"),
        }
        .record(p));
    }
    for index in 0..count {
        let a = p.rd16(crt_wide_unit_address(left, index)?)?;
        let b = p.rd16(crt_wide_unit_address(right, index)?)?;
        if a != b {
            return Ok(a.cmp(&b) as i32 as u32 as u64);
        }
        if a == 0 {
            return Ok(0);
        }
    }
    Ok(0)
}

fn crt_wcsrchr(p: &mut Process) -> R {
    let (string, character) = (p.arg(0), p.arg(1) as u16);
    if string == 0 {
        crt_invalid_parameter(p, "wcsrchr")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(0);
    }
    let mut last = 0;
    for index in 0..CRT_WIDE_STRING_MAX_UNITS {
        let address = crt_wide_unit_address(string, index)?;
        let unit = p.rd16(address)?;
        if unit == character {
            last = address;
        }
        if unit == 0 {
            return Ok(last);
        }
    }
    Err(Stop::Crash(format!(
        "unterminated wcsrchr string at {string:#x} exceeds {CRT_WIDE_STRING_MAX_UNITS} UTF-16 code units"
    )))
}

fn crt_secure_wide_string_error(
    p: &mut Process,
    function: &str,
    destination: u64,
    capacity: u64,
    error: u32,
) -> Result<u64, Stop> {
    if destination != 0 && capacity != 0 {
        p.wr16(destination, 0)?;
    }
    let errno = errno_cell(p);
    p.wr32(errno, error)?;
    crt_invalid_parameter(p, function)?;
    Ok(error as u64)
}

fn crt_secure_wide_copy(p: &mut Process, append: bool) -> R {
    let (destination, capacity, source) = (p.arg(0), p.arg(1), p.arg(2));
    let function = if append { "wcscat_s" } else { "wcscpy_s" };
    if destination == 0 || source == 0 || capacity == 0 {
        return crt_secure_wide_string_error(p, function, destination, capacity, 22);
    }
    if capacity > CRT_WIDE_STRING_MAX_UNITS {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("{function} capacity {capacity} exceeds the translated code-unit limit"),
        }
        .record(p));
    }

    let start = if append {
        let mut index = 0;
        while index < capacity && p.rd16(crt_wide_unit_address(destination, index)?)? != 0 {
            index += 1;
        }
        if index == capacity {
            return crt_secure_wide_string_error(p, function, destination, capacity, 22);
        }
        index
    } else {
        0
    };
    let mut source_units = Vec::new();
    for index in 0..capacity {
        let unit = p.rd16(crt_wide_unit_address(source, index)?)?;
        if unit == 0 {
            source_units.push(0);
            let required = start + source_units.len() as u64;
            if required > capacity {
                return crt_secure_wide_string_error(p, function, destination, capacity, 34);
            }
            for (offset, unit) in source_units.into_iter().enumerate() {
                p.wr16(
                    crt_wide_unit_address(destination, start + offset as u64)?,
                    unit,
                )?;
            }
            return Ok(0);
        }
        source_units.push(unit);
    }
    // A source terminator beyond the advertised destination capacity cannot fit.
    crt_secure_wide_string_error(p, function, destination, capacity, 34)
}

fn crt_parse_wide_integer(units: &[u16], base: u32, unsigned: bool) -> (u32, usize, bool, bool) {
    let mut cursor = 0usize;
    while units
        .get(cursor)
        .is_some_and(|unit| matches!(*unit, 0x20 | 0x09..=0x0d))
    {
        cursor += 1;
    }
    let negative = match units.get(cursor).copied() {
        Some(0x2d) => {
            cursor += 1;
            true
        }
        Some(0x2b) => {
            cursor += 1;
            false
        }
        _ => false,
    };
    let mut radix = base;
    let has_hex_prefix = units.get(cursor) == Some(&(b'0' as u16))
        && units
            .get(cursor + 1)
            .is_some_and(|unit| matches!(*unit, 0x58 | 0x78))
        && units
            .get(cursor + 2)
            .is_some_and(|unit| matches!(*unit, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66));
    if radix == 0 {
        if has_hex_prefix {
            radix = 16;
            cursor += 2;
        } else if units.get(cursor) == Some(&(b'0' as u16)) {
            radix = 8;
        } else {
            radix = 10;
        }
    } else if radix == 16 && has_hex_prefix {
        cursor += 2;
    }
    let digit_start = cursor;
    let limit = if unsigned {
        u32::MAX as u64
    } else if negative {
        2_147_483_648u64
    } else {
        i32::MAX as u64
    };
    let mut magnitude = 0u64;
    let mut overflow = false;
    while let Some(&unit) = units.get(cursor) {
        let digit = match unit {
            0x30..=0x39 => (unit - 0x30) as u32,
            0x41..=0x5a => (unit - 0x41 + 10) as u32,
            0x61..=0x7a => (unit - 0x61 + 10) as u32,
            _ => break,
        };
        if digit >= radix {
            break;
        }
        if magnitude > (limit - digit as u64) / radix as u64 {
            overflow = true;
        } else if !overflow {
            magnitude = magnitude * radix as u64 + digit as u64;
        }
        cursor += 1;
    }
    if cursor == digit_start {
        return (0, 0, false, false);
    }
    let value = if overflow {
        limit as u32
    } else if unsigned && negative {
        (0u32).wrapping_sub(magnitude as u32)
    } else if negative {
        (-(magnitude as i64)) as i32 as u32
    } else {
        magnitude as u32
    };
    (value, cursor, overflow, true)
}

fn crt_wcstol_common(p: &mut Process, unsigned: bool) -> R {
    let (string, end_pointer, base) = (p.arg(0), p.arg(1), p.arg(2) as u32);
    if string == 0 {
        crt_invalid_parameter(p, if unsigned { "wcstoul" } else { "wcstol" })?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        if end_pointer != 0 {
            p.wrptr(end_pointer, 0)?;
        }
        return Ok(0);
    }
    let original = read_crt_wide_units(p, string)?;
    let invalid_base = base != 0 && !(2..=36).contains(&base);
    if invalid_base {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        if end_pointer != 0 {
            p.wrptr(end_pointer, string)?;
        }
        return Ok(0);
    }
    let (value, used, overflow, any) = crt_parse_wide_integer(&original, base, unsigned);
    if end_pointer != 0 {
        let offset = if any { used as u64 * 2 } else { 0 };
        p.wrptr(end_pointer, string + offset)?;
    }
    if overflow {
        let errno = errno_cell(p);
        p.wr32(errno, 34)?; // ERANGE
    }
    Ok(value as u64)
}

fn crt_wcstod(p: &mut Process) -> R {
    let (string, end_pointer) = (p.arg(0), p.arg(1));
    if string == 0 {
        crt_invalid_parameter(p, "wcstod")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        if end_pointer != 0 {
            p.wrptr(end_pointer, 0)?;
        }
        crt_set_double_return(p, 0.0);
        return Ok(0);
    }
    let units = read_crt_wide_units(p, string)?;
    let text: String = units
        .iter()
        .map(|&unit| {
            char::from_u32(unit as u32)
                .filter(|ch| ch.is_ascii())
                .unwrap_or('\u{fffd}')
        })
        .collect();
    let (value, used) = parse_float(&text);
    if end_pointer != 0 {
        p.wrptr(end_pointer, string + used as u64 * 2)?;
    }
    crt_set_double_return(p, value);
    Ok(0)
}

fn crt_atoflt(p: &mut Process) -> R {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_atoflt outside the verified PE32 ABI".into(),
        }
        .record(p));
    }
    let result = p.arg(0);
    let input = p.arg(1);
    if result == 0 || input == 0 {
        return Err(Stop::Crash("_atoflt received a null argument".into()));
    }

    let text = p.read_cstr(input)?;
    let (value, used) = parse_float(&text);
    let parsed = &text[..used.min(text.len())];
    let lower = parsed
        .trim_start()
        .trim_start_matches(['+', '-'])
        .to_ascii_lowercase();
    let explicit_nonfinite = lower.starts_with("inf") || lower.starts_with("nan");
    let nonzero_decimal = parsed
        .bytes()
        .take_while(|byte| !matches!(byte, b'e' | b'E'))
        .any(|byte| byte.is_ascii_digit() && byte != b'0');
    let converted = value as f32;
    let status =
        if used != 0 && !explicit_nonfinite && (value.is_infinite() || converted.is_infinite()) {
            3 // VC80 INTRNCVT_OVERFLOW
        } else if used != 0
            && !explicit_nonfinite
            && nonzero_decimal
            && (converted == 0.0 || converted.abs() < f32::MIN_POSITIVE)
        {
            4 // VC80 INTRNCVT_UNDERFLOW
        } else {
            0
        };
    p.wr32(result, converted.to_bits())?;
    Ok(status)
}

fn crt_set_double_return(p: &mut Process, value: f64) {
    if p.cpu.mode == CpuMode::X86_32 {
        let top = p.cpu.fpu_top as usize & 7;
        p.cpu.fpu_st[top] = value;
        p.cpu.fpu_tw &= !(3 << (2 * top));
    } else {
        p.set_retf(value);
    }
}

fn crt_mbstowcs(p: &mut Process) -> R {
    let (destination, source, count) = (p.arg(0), p.arg(1), p.arg(2) as usize);
    if source == 0 {
        crt_invalid_parameter(p, "mbstowcs")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(u32::MAX as u64);
    }
    let bytes = p.read_cstr_raw(source)?;
    let units: Vec<u16> = bytes.iter().map(|&byte| kernel32::cp1252(byte)).collect();
    if destination == 0 {
        if count == 0 {
            return Ok(units.len() as u64);
        }
        crt_invalid_parameter(p, "mbstowcs")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(u32::MAX as u64);
    }
    let copied = units.len().min(count);
    for (index, unit) in units.iter().take(copied).enumerate() {
        p.wr16(crt_wide_unit_address(destination, index as u64)?, *unit)?;
    }
    if copied == units.len() && copied < count {
        p.wr16(crt_wide_unit_address(destination, copied as u64)?, 0)?;
    }
    Ok(copied as u64)
}

fn crt_wcstombs(p: &mut Process) -> R {
    let (destination, source, count) = (p.arg(0), p.arg(1), p.arg(2) as usize);
    if source == 0 {
        crt_invalid_parameter(p, "wcstombs")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(u32::MAX as u64);
    }
    let units = read_crt_wide_units(p, source)?;
    if destination == 0 {
        if count == 0 {
            let mut required = 0u64;
            for unit in units {
                if crt_cp1252_encode_unit(unit).is_none() {
                    let errno = errno_cell(p);
                    p.wr32(errno, 42)?; // EILSEQ
                    return Ok(u64::MAX);
                }
                required += 1;
            }
            return Ok(required);
        }
        crt_invalid_parameter(p, "wcstombs")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(u32::MAX as u64);
    }
    let mut copied = 0usize;
    for unit in units {
        if copied == count {
            break;
        }
        let Some(byte) = crt_cp1252_encode_unit(unit) else {
            let errno = errno_cell(p);
            p.wr32(errno, 42)?; // EILSEQ
            return Ok(u64::MAX);
        };
        p.wr8(destination + copied as u64, byte)?;
        copied += 1;
    }
    if copied < count {
        p.wr8(destination + copied as u64, 0)?;
    }
    Ok(copied as u64)
}

fn crt_mbtowc(p: &mut Process) -> R {
    let (destination, source, count) = (p.arg(0), p.arg(1), p.arg(2));
    if source == 0 {
        return Ok(0);
    }
    if count == 0 {
        return Ok(u32::MAX as u64);
    }
    let byte = p.rd8(source)?;
    if destination != 0 {
        p.wr16(destination, kernel32::cp1252(byte))?;
    }
    Ok(if byte == 0 { 0 } else { 1 })
}

fn crt_wctomb(p: &mut Process) -> R {
    let (destination, character) = (p.arg(0), p.arg(1) as u16);
    if destination == 0 {
        return Ok(0);
    }
    let Some(byte) = crt_cp1252_encode_unit(character) else {
        let errno = errno_cell(p);
        p.wr32(errno, 42)?; // EILSEQ
        return Ok(u32::MAX as u64);
    };
    p.wr8(destination, byte)?;
    Ok(1)
}

fn crt_cp1252_encode_unit(unit: u16) -> Option<u8> {
    if unit <= 0x7f || (0xa0..=0xff).contains(&unit) {
        Some(unit as u8)
    } else {
        (0x80u8..=0x9f).find(|&byte| kernel32::cp1252(byte) == unit)
    }
}

fn c_string_byte_set(bytes: &[u8]) -> [bool; 256] {
    let mut set = [false; 256];
    for &byte in bytes {
        set[byte as usize] = true;
    }
    set
}

fn crt_strcoll(p: &mut Process) -> R {
    let (left, right) = (p.read_cstr_raw(p.arg(0))?, p.read_cstr_raw(p.arg(1))?);
    Ok(left.cmp(&right) as i32 as i64 as u64)
}

fn crt_getdrive(p: &mut Process) -> R {
    let path = p.current_directory.as_bytes();
    let Some(&letter) = path.first() else {
        return Ok(0);
    };
    if path.get(1) != Some(&b':') {
        return Ok(0);
    }
    let letter = letter.to_ascii_uppercase();
    if !(b'A'..=b'Z').contains(&letter) {
        return Ok(0);
    }
    Ok((letter - b'A' + 1) as u64)
}

fn parse_wide_integer(input: &[u16], conversion: u8) -> Option<(u64, usize)> {
    let mut index = 0usize;
    let negative = match input.first().copied() {
        Some(unit) if unit == b'+' as u16 => {
            index = 1;
            false
        }
        Some(unit) if unit == b'-' as u16 => {
            index = 1;
            true
        }
        _ => false,
    };

    let base = match conversion {
        b'o' => 8,
        b'x' | b'X' => 16,
        b'i' => match (input.get(index), input.get(index + 1)) {
            (Some(&zero), Some(&prefix))
                if zero == b'0' as u16 && (prefix == b'x' as u16 || prefix == b'X' as u16) =>
            {
                16
            }
            (Some(&zero), _) if zero == b'0' as u16 => 8,
            _ => 10,
        },
        _ => 10,
    };

    // A 0x prefix is part of the item only when followed by at least one hex
    // digit. This matches scanf's longest valid integer prefix behavior.
    if base == 16
        && input.get(index) == Some(&(b'0' as u16))
        && input
            .get(index + 1)
            .is_some_and(|unit| *unit == b'x' as u16 || *unit == b'X' as u16)
        && input
            .get(index + 2)
            .and_then(|unit| wide_digit(*unit))
            .is_some_and(|d| d < 16)
    {
        index += 2;
    }

    let first_digit = index;
    let mut value = 0u64;
    while let Some(unit) = input.get(index).copied() {
        let Some(digit) = wide_digit(unit) else {
            break;
        };
        if digit >= base {
            break;
        }
        value = value.wrapping_mul(base as u64).wrapping_add(digit as u64);
        index += 1;
    }
    if index == first_digit {
        return None;
    }
    Some((
        if negative {
            value.wrapping_neg()
        } else {
            value
        },
        index,
    ))
}

fn wide_digit(unit: u16) -> Option<u32> {
    match unit {
        unit if (b'0' as u16..=b'9' as u16).contains(&unit) => Some((unit - b'0' as u16) as u32),
        unit if (b'a' as u16..=b'f' as u16).contains(&unit) => {
            Some((unit - b'a' as u16) as u32 + 10)
        }
        unit if (b'A' as u16..=b'F' as u16).contains(&unit) => {
            Some((unit - b'A' as u16) as u32 + 10)
        }
        _ => None,
    }
}

fn write_swscanf_integer(
    p: &mut Process,
    destination: u64,
    value: u64,
    length_modifier: u8,
) -> Result<(), Stop> {
    match length_modifier {
        3 => p.wr8(destination, value as u8),   // hh
        4 => p.wr16(destination, value as u16), // h
        2 => p.wr64(destination, value),        // ll / I64
        _ => p.wr32(destination, value as u32), // int and Windows long are 32-bit
    }
}

fn crt_swscanf(p: &mut Process) -> R {
    let input = read_crt_wide_units(p, p.arg(0))?;
    let format = read_crt_wide_units(p, p.arg(1))?;
    let mut input_index = 0usize;
    let mut format_index = 0usize;
    let mut argument_index = 0usize;
    let mut assignments = 0u64;
    let mut attempted_conversion = false;

    while format_index < format.len() {
        let format_unit = format[format_index];
        if format_unit <= u8::MAX as u16 && (format_unit as u8).is_ascii_whitespace() {
            while format_index < format.len()
                && format[format_index] <= u8::MAX as u16
                && (format[format_index] as u8).is_ascii_whitespace()
            {
                format_index += 1;
            }
            while input_index < input.len()
                && input[input_index] <= u8::MAX as u16
                && (input[input_index] as u8).is_ascii_whitespace()
            {
                input_index += 1;
            }
            continue;
        }

        if format_unit != b'%' as u16 {
            if input.get(input_index) != Some(&format_unit) {
                break;
            }
            input_index += 1;
            format_index += 1;
            continue;
        }

        format_index += 1;
        if format.get(format_index) == Some(&(b'%' as u16)) {
            if input.get(input_index) != Some(&(b'%' as u16)) {
                break;
            }
            input_index += 1;
            format_index += 1;
            continue;
        }

        let suppressed = format.get(format_index) == Some(&(b'*' as u16));
        if suppressed {
            format_index += 1;
        }

        let width_start = format_index;
        let mut width = 0usize;
        while let Some(unit) = format.get(format_index).copied() {
            if !(b'0' as u16..=b'9' as u16).contains(&unit) {
                break;
            }
            width = width
                .saturating_mul(10)
                .saturating_add((unit - b'0' as u16) as usize);
            format_index += 1;
        }
        let width = (format_index != width_start).then_some(width);

        let mut length_modifier = 0u8;
        match format.get(format_index).copied() {
            Some(unit) if unit == b'h' as u16 => {
                format_index += 1;
                if format.get(format_index) == Some(&(b'h' as u16)) {
                    length_modifier = 3; // hh
                    format_index += 1;
                } else {
                    length_modifier = 4; // h
                }
            }
            Some(unit) if unit == b'l' as u16 || unit == b'L' as u16 => {
                length_modifier = 1;
                if unit == b'L' as u16 {
                    length_modifier = 2;
                }
                format_index += 1;
                if format.get(format_index) == Some(&(b'l' as u16)) {
                    length_modifier = 2; // ll
                    format_index += 1;
                }
            }
            Some(unit) if unit == b'I' as u16 => {
                format_index += 1;
                let modifier_start = format_index;
                while format
                    .get(format_index)
                    .is_some_and(|unit| (b'0' as u16..=b'9' as u16).contains(unit))
                {
                    format_index += 1;
                }
                length_modifier = match &format[modifier_start..format_index] {
                    [a, b] if *a == b'3' as u16 && *b == b'2' as u16 => 5, // I32
                    [a, b] if *a == b'6' as u16 && *b == b'4' as u16 => 2, // I64
                    [] => 0, // pointer-sized integer; x86 guest pointers are 32-bit
                    _ => {
                        return Err(Stop::Unimplemented {
                            dll: "crt".into(),
                            name: "swscanf unsupported I-length modifier".into(),
                        }
                        .record(p));
                    }
                };
            }
            _ => {}
        }

        let Some(conversion) = format.get(format_index).copied() else {
            break;
        };
        format_index += 1;
        let Some(conversion) = u8::try_from(conversion).ok() else {
            return Err(Stop::Unimplemented {
                dll: "crt".into(),
                name: "swscanf non-ASCII format conversion".into(),
            }
            .record(p));
        };
        if !matches!(
            conversion,
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'd' | b'i' | b'u' | b'o' | b'x' | b'X'
        ) {
            return Err(Stop::Unimplemented {
                dll: "crt".into(),
                name: format!("swscanf %{} conversion", conversion as char),
            }
            .record(p));
        }
        attempted_conversion = true;
        while input_index < input.len()
            && input[input_index] <= u8::MAX as u16
            && (input[input_index] as u8).is_ascii_whitespace()
        {
            input_index += 1;
        }
        let available = &input[input_index..];
        let available = match width {
            Some(width) => &available[..available.len().min(width)],
            None => available,
        };
        if matches!(conversion, b'd' | b'i' | b'u' | b'o' | b'x' | b'X') {
            let Some((value, consumed)) = parse_wide_integer(available, conversion) else {
                break;
            };
            if !suppressed {
                let destination = p.arg(2 + argument_index);
                argument_index += 1;
                write_swscanf_integer(p, destination, value, length_modifier)?;
                assignments += 1;
            }
            input_index += consumed;
            continue;
        }
        let ascii = available
            .iter()
            .take_while(|&&unit| unit <= 0x7f)
            .map(|&unit| unit as u8)
            .collect::<Vec<_>>();
        let text = String::from_utf8_lossy(&ascii);
        let (value, consumed) = parse_float(&text);
        if consumed == 0 {
            break;
        }

        if !suppressed {
            let destination = p.arg(2 + argument_index);
            argument_index += 1;
            if length_modifier == 1 || length_modifier == 2 {
                p.wr64(destination, value.to_bits())?;
            } else {
                p.wr32(destination, (value as f32).to_bits())?;
            }
            assignments += 1;
        }
        input_index += consumed;
    }

    if assignments == 0 && attempted_conversion && input_index >= input.len() {
        return Ok(u32::MAX as u64); // EOF before the first assignment
    }
    Ok(assignments)
}

/// MSVC `_getcwd` / `_wgetcwd`: return the supplied directory in the caller's buffer,
/// or allocate a CRT heap buffer when the caller passes null.
fn crt_getcwd_for_directory(
    p: &mut Process,
    wide: bool,
    function: &str,
    buffer: u64,
    maxlen: i32,
    directory: &str,
) -> R {
    if maxlen <= 0 {
        crt_invalid_parameter(p, function)?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }

    let wide_path = directory.encode_utf16().collect::<Vec<_>>();
    let narrow_path = (!wide).then(|| kernel32::cp_encode(1252, &wide_path));
    let required = if wide {
        wide_path.len().saturating_add(1)
    } else {
        narrow_path.as_ref().unwrap().len().saturating_add(1)
    };
    if required > maxlen as usize {
        let errno = errno_cell(p);
        p.wr32(errno, 34)?; // ERANGE
        return Ok(0);
    }

    let output = if buffer == 0 {
        // Guard the host-side zeroing allocation while preserving malloc's ENOMEM result.
        const MAX_GETCWD_ALLOC_UNITS: u64 = 1 << 20;
        if maxlen as u64 > MAX_GETCWD_ALLOC_UNITS {
            let errno = errno_cell(p);
            p.wr32(errno, 12)?; // ENOMEM
            return Ok(0);
        }
        let width = if wide { 2 } else { 1 };
        let Some(bytes) = (maxlen as u64).checked_mul(width) else {
            let errno = errno_cell(p);
            p.wr32(errno, 12)?; // ENOMEM
            return Ok(0);
        };
        let allocated = kernel32::alloc_block(p, bytes, true);
        if allocated == 0 {
            let errno = errno_cell(p);
            p.wr32(errno, 12)?; // ENOMEM
            return Ok(0);
        }
        allocated
    } else {
        buffer
    };

    if wide {
        p.write_wstr(output, &directory)?;
    } else {
        let mut bytes = narrow_path.unwrap();
        bytes.push(0);
        p.write_bytes(output, &bytes)?;
    }
    Ok(output)
}

/// MSVC `_getcwd` / `_wgetcwd`: return the guest's current directory.
fn crt_getcwd(p: &mut Process, wide: bool) -> R {
    let (buffer, maxlen) = (p.arg(0), p.arg(1) as u32 as i32);
    let directory = p.current_directory.clone();
    crt_getcwd_for_directory(
        p,
        wide,
        if wide { "_wgetcwd" } else { "_getcwd" },
        buffer,
        maxlen,
        &directory,
    )
}

/// MSVC `_getdcwd` / `_wgetdcwd`: the process model stores the current directory
/// for the active drive, so requests for drive zero or that drive have real results.
/// A different drive has no modeled per-drive directory and follows CRT validation.
fn crt_getdcwd(p: &mut Process, wide: bool) -> R {
    let drive = p.arg(0) as u32 as i32;
    let buffer = p.arg(1);
    let maxlen = p.arg(2) as u32 as i32;
    let function = if wide { "_wgetdcwd" } else { "_getdcwd" };

    let current_drive = crt_getdrive(p)? as i32;
    if drive < 0 || (drive != 0 && drive != current_drive) {
        crt_invalid_parameter(p, function)?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL for an unavailable or unmodeled drive.
        return Ok(0);
    }

    let directory = p.current_directory.clone();
    crt_getcwd_for_directory(p, wide, function, buffer, maxlen, &directory)
}

/// MSVC `_wtol` parses a decimal prefix and returns a Windows 32-bit `long`.
fn crt_wtol(p: &mut Process) -> R {
    let input = p.arg(0);
    if input == 0 {
        crt_invalid_parameter(p, "_wtol")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }
    let units = read_crt_wide_units(p, input)?;
    let mut cursor = 0;
    while units
        .get(cursor)
        .is_some_and(|unit| matches!(*unit, 0x20 | 0x09))
    {
        cursor += 1;
    }
    let negative = match units.get(cursor) {
        Some(0x2d) => {
            cursor += 1;
            true
        }
        Some(0x2b) => {
            cursor += 1;
            false
        }
        _ => false,
    };
    let digit_start = cursor;
    let limit = if negative {
        2_147_483_648u64
    } else {
        2_147_483_647u64
    };
    let mut magnitude = 0u64;
    let mut overflow = false;
    while let Some(unit @ 0x30..=0x39) = units.get(cursor).copied() {
        let digit = (unit - 0x30) as u64;
        if magnitude > (limit - digit) / 10 {
            overflow = true;
        } else if !overflow {
            magnitude = magnitude * 10 + digit;
        }
        cursor += 1;
    }
    if cursor == digit_start {
        return Ok(0);
    }
    if overflow {
        let errno = errno_cell(p);
        p.wr32(errno, 34)?; // ERANGE
        return Ok(if negative {
            i32::MIN as u32 as u64
        } else {
            i32::MAX as u32 as u64
        });
    }
    let value = if negative {
        -(magnitude as i64)
    } else {
        magnitude as i64
    };
    Ok(value as i32 as u32 as u64)
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "_wgetcwd" => |p| crt_getcwd(p, true),
        "_getcwd" | "getcwd" => |p| crt_getcwd(p, false),
        "_wgetdcwd" => |p| crt_getdcwd(p, true),
        "_getdcwd" => |p| crt_getdcwd(p, false),
        "_getdrive" => crt_getdrive,
        "_wstat64i32" => crt_wstat64i32,
        "_stat64i32" => crt_stat64i32,
        "_fstat64i32" => crt_fstat64i32,
        "_wutime64" => crt_wutime64,
        "_wrename" => crt_wrename,
        "_wsearchenv" => crt_wsearchenv,
        "_open" => crt_open,
        "_read" => crt_read,
        "_close" => crt_close,
        "_lseek" => crt_lseek,
        "_setmode" => crt_setmode,
        "_get_osfhandle" => crt_get_osfhandle,
        "_wtol" => crt_wtol,
        "_configthreadlocale" => crt_configthreadlocale,
        "_setmbcp" => crt_setmbcp,
        "_mktemp" => crt_mktemp,
        "_ecvt" => |p| crt_ecvt_fcvt(p, false),
        "_fcvt" => |p| crt_ecvt_fcvt(p, true),
        "setvbuf" => crt_setvbuf,
        "tmpnam" => |p| crt_untranslated_import(p, "tmpnam", "temporary-directory policy"),
        "tmpfile" => |p| crt_untranslated_import(p, "tmpfile", "delete-on-close VFS handle"),
        "_popen" => |p| crt_untranslated_import(p, "_popen", "shell-pipe process creation"),
        "_pclose" => |p| crt_untranslated_import(p, "_pclose", "shell-pipe wait status"),
        "_wspawnv" => |p| crt_untranslated_import(p, "_wspawnv", "child-process creation and wait status"),
        "fscanf" => |p| crt_untranslated_import(p, "fscanf", "stream position after partial conversions"),
        "__dllonexit" => |p| crt_untranslated_import(p, "__dllonexit", "per-module callback table growth"),
        "__setusermatherr" => crt_setusermatherr,
        "_invoke_watson" => |p| crt_untranslated_import(p, "_invoke_watson", "Windows fail-fast reporting"),
        "_crt_debugger_hook" => |p| crt_untranslated_import(p, "_crt_debugger_hook", "debugger trap behavior"),
        "?terminate@@YAXXZ" => |p| crt_untranslated_import(p, "?terminate@@YAXXZ", "terminate handler registration"),
        "?_type_info_dtor_internal_method@type_info@@QAEXXZ" => |p| {
            crt_untranslated_import(
                p,
                "?_type_info_dtor_internal_method@type_info@@QAEXXZ",
                "type_info destructor ownership and deallocation",
            )
        },
        "_CIsin" => |p| crt_ci_unary(p, "sin"),
        "_CIsqrt" => |p| crt_ci_unary(p, "sqrt"),
        "_CIcos" => |p| crt_ci_unary(p, "cos"),
        "_CItan" => |p| crt_ci_unary(p, "tan"),
        "_CIsinh" => |p| crt_ci_unary(p, "sinh"),
        "_CIcosh" => |p| crt_ci_unary(p, "cosh"),
        "_CItanh" => |p| crt_ci_unary(p, "tanh"),
        "_CIatan" => |p| crt_ci_unary(p, "atan"),
        "_CIexp" => |p| crt_ci_unary(p, "exp"),
        "_CIlog" => |p| crt_ci_unary(p, "log"),
        "_CIlog10" => |p| crt_ci_unary(p, "log10"),
        "_CIacos" => |p| crt_ci_unary(p, "acos"),
        "_CIasin" => |p| crt_ci_unary(p, "asin"),
        "_CIfmod" | "_CIatan2" | "_CIpow" => |p| {
            Err(Stop::Unimplemented {
                dll: "crt".into(),
                name: "x87 two-operand _CI math calling convention".into(),
            }
            .record(p))
        },
        "_controlfp_s" => controlfp_s,
        "_controlfp" => controlfp_compat,
        "_control87" => control87,
        "__set_app_type" | "_set_app_type" => |p| {
            // MSVC startup records whether the image is a console or GUI application.
            // The value is process state, not a placeholder success result.
            p.crt_app_type = p.arg(0) as u32;
            Ok(0)
        },
        "wcsncpy" => crt_wcsncpy,
        "_wcslwr" | "wcslwr" => crt_wcslwr,
        "_wcsicmp" | "wcsicmp" => crt_wcsicmp,
        "_wcsnicmp" | "wcsnicmp" => crt_wcsnicmp,
        "wcsstr" => crt_wcsstr,
        "wcschr" => crt_wcschr,
        "wcspbrk" => crt_wcspbrk,
        "wcsncmp" => crt_wcsncmp,
        "wcsrchr" => crt_wcsrchr,
        "wcscpy_s" => |p| crt_secure_wide_copy(p, false),
        "wcscat_s" => |p| crt_secure_wide_copy(p, true),
        "wcstol" => |p| crt_wcstol_common(p, false),
        "wcstoul" => |p| crt_wcstol_common(p, true),
        "wcstod" => crt_wcstod,
        "mbstowcs" => crt_mbstowcs,
        "wcstombs" => crt_wcstombs,
        "mbtowc" => crt_mbtowc,
        "wctomb" => crt_wctomb,
        "strcoll" => crt_strcoll,
        "swscanf" => crt_swscanf,
        "__anywasm_basic_streambuf_char_lock_noop"
        | "?_Lock@?$basic_streambuf@DU?$char_traits@D@std@@@std@@UEAAXXZ"
        | "?_Unlock@?$basic_streambuf@DU?$char_traits@D@std@@@std@@UEAAXXZ"
        | "?_Lock@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@UEAAXXZ"
        | "?_Unlock@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@UEAAXXZ" => {
            msvc_basic_streambuf_char_lock_noop
        }
        "?showmanyc@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAA_JXZ"
        | "?showmanyc@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAA_JXZ" => {
            msvc_basic_streambuf_showmanyc
        }
        "?setbuf@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAAPEAV12@PEAD_J@Z"
        | "?setbuf@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAAPEAV12@PEA_W_J@Z" => {
            msvc_basic_streambuf_setbuf
        }
        "?sync@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAAHXZ"
        | "?sync@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAAHXZ" => {
            msvc_basic_streambuf_char_sync
        }
        "?imbue@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAAXAEBVlocale@2@@Z"
        | "?imbue@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAAXAEBVlocale@2@@Z" => {
            msvc_basic_streambuf_imbue
        }
        "?sputn@?$basic_streambuf@DU?$char_traits@D@std@@@std@@QEAA_JPEBD_J@Z" => {
            msvc_basic_streambuf_char_sputn
        }
        "?sputc@?$basic_streambuf@DU?$char_traits@D@std@@@std@@QEAAHD@Z" => {
            msvc_basic_streambuf_char_sputc
        }
        "__anywasm_basic_streambuf_char_xsputn"
        | "?xsputn@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAA_JPEBD_J@Z" => {
            msvc_basic_streambuf_char_xsputn
        }
        "__anywasm_basic_streambuf_wchar_xsputn"
        | "?xsputn@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAA_JPEB_W_J@Z" => {
            msvc_basic_streambuf_wchar_xsputn
        }
        "__anywasm_basic_streambuf_char_underflow"
        | "?underflow@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAAHXZ" => {
            msvc_basic_streambuf_char_underflow
        }
        "__anywasm_basic_streambuf_wchar_underflow"
        | "?underflow@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAAGXZ" => {
            msvc_basic_streambuf_wchar_underflow
        }
        "__anywasm_basic_streambuf_char_uflow"
        | "?uflow@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAAHXZ" => {
            msvc_basic_streambuf_char_uflow
        }
        "__anywasm_basic_streambuf_wchar_uflow"
        | "?uflow@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAAGXZ" => {
            msvc_basic_streambuf_wchar_uflow
        }
        "__anywasm_basic_streambuf_char_xsgetn"
        | "?xsgetn@?$basic_streambuf@DU?$char_traits@D@std@@@std@@MEAA_JPEAD_J@Z" => {
            msvc_basic_streambuf_char_xsgetn
        }
        "__anywasm_basic_streambuf_wchar_xsgetn"
        | "?xsgetn@?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@MEAA_JPEA_W_J@Z" => {
            msvc_basic_streambuf_wchar_xsgetn
        }
        "__anywasm_basic_streambuf_char_sync" => msvc_basic_streambuf_char_sync,
        "__anywasm_standard_streambuf_char_overflow" => msvc_standard_streambuf_char_overflow,
        "__anywasm_standard_streambuf_xsputn" => msvc_standard_streambuf_xsputn,
        "__anywasm_standard_streambuf_sync" => msvc_standard_streambuf_sync,
        "?length@?$char_traits@D@std@@SAIPBD@Z" => msvc80_x86_char_traits_length,
        "?eof@?$char_traits@D@std@@SAHXZ" => msvc80_x86_char_traits_eof,
        "?eq_int_type@?$char_traits@D@std@@SA_NABH0@Z" => {
            msvc80_x86_char_traits_eq_int_type
        }
        "?sputc@?$basic_streambuf@DU?$char_traits@D@std@@@std@@QAEHD@Z" => {
            msvc80_x86_basic_streambuf_sputc
        }
        "?sputn@?$basic_streambuf@DU?$char_traits@D@std@@@std@@QAEHPBDH@Z" => {
            msvc80_x86_basic_streambuf_sputn
        }
        "?setstate@?$basic_ios@DU?$char_traits@D@std@@@std@@QAEXH_N@Z" => {
            msvc80_x86_basic_ios_setstate
        }
        "?_Osfx@?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEXXZ" => {
            msvc80_x86_ostream_osfx_import
        }
        "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV01@H@Z" => {
            msvc80_x86_ostream_insert_int
        }
        "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV01@P6AAAV01@AAV01@@Z@Z" => {
            msvc80_x86_ostream_insert_manipulator
        }
        "?_Lock@_Mutex@std@@QAEXXZ" => msvc80_x86_mutex_lock,
        "?_Unlock@_Mutex@std@@QAEXXZ" => msvc80_x86_mutex_unlock,
        "?read@?$basic_istream@DU?$char_traits@D@std@@@std@@QAEAAV12@PADH@Z" => {
            msvc80_x86_istream_read
        }
        "?write@?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV12@PBDH@Z" => {
            msvc80_x86_ostream_write
        }
        "?flush@?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV12@XZ" => {
            msvc80_x86_ostream_flush_import
        }
        "?endl@std@@YAAAV?$basic_ostream@DU?$char_traits@D@std@@@1@AAV21@@Z" => {
            msvc80_x86_ostream_endl
        }
        "??1?$basic_streambuf@DU?$char_traits@D@std@@@std@@UEAA@XZ" => {
            msvc_basic_streambuf_char_destructor
        }
        "??1?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@UEAA@XZ" => {
            msvc_basic_streambuf_wchar_destructor
        }
        "??1?$basic_iostream@DU?$char_traits@D@std@@@std@@UEAA@XZ" => {
            msvc_basic_iostream_char_destructor
        }
        "??1?$basic_iostream@_WU?$char_traits@_W@std@@@std@@UEAA@XZ" => {
            msvc_basic_iostream_wchar_destructor
        }
        "??1?$basic_ios@DU?$char_traits@D@std@@@std@@UEAA@XZ" => {
            msvc_basic_ios_char_destructor
        }
        "??1?$basic_ios@_WU?$char_traits@_W@std@@@std@@UEAA@XZ" => {
            msvc_basic_ios_char_destructor
        }
        "?_Osfx@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAXXZ" => {
            msvc_basic_ostream_char_osfx
        }
        "?flush@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@XZ" => {
            msvc_standard_ostream_flush
        }
        "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV01@I@Z" => {
            msvc_ostream_insert_u32
        }
        "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV01@H@Z" => {
            msvc_ostream_insert_i32
        }
        "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV01@G@Z" => {
            msvc_ostream_insert_u16
        }
        "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV01@_K@Z" => {
            msvc_ostream_insert_u64
        }
        "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV01@M@Z" => {
            msvc_ostream_insert_float
        }
        "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV01@N@Z" => {
            msvc_ostream_insert_double
        }
        "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV01@P6AAEAVios_base@1@AEAV21@@Z@Z" => {
            msvc_ostream_insert_ios_base_manipulator
        }
        "?setprecision@std@@YA?AU?$_Smanip@_J@1@_J@Z" => msvc_setprecision,
        "?setw@std@@YA?AU?$_Smanip@_J@1@_J@Z" => msvc_setw,
        "__anywasm_ios_base_set_precision" => msvc_ios_base_set_precision,
        "__anywasm_ios_base_set_width" => msvc_ios_base_set_width,
        "?good@ios_base@std@@QEBA_NXZ" => msvc_ios_base_good,
        "?getloc@ios_base@std@@QEBA?AVlocale@2@XZ" => msvc_ios_base_getloc,
        "?_Getgloballocale@locale@std@@CAPEAV_Locimp@12@XZ" => msvc_get_global_locale,
        "__anywasm_locale_facet_incref" => msvc_locale_facet_incref,
        "__anywasm_locale_facet_decref" => msvc_locale_facet_decref,
        "__anywasm_locale_facet_destructor" => msvc_locale_facet_destructor,
        "?_Getcat@?$ctype@_W@std@@SA_KPEAPEBVfacet@locale@2@PEBV42@@Z" => {
            msvc_ctype_wchar_getcat
        }
        "?widen@?$ctype@_W@std@@QEBA_WD@Z" => msvc_ctype_wchar_widen,
        "?setstate@?$basic_ios@DU?$char_traits@D@std@@@std@@QEAAXH_N@Z" => {
            msvc_basic_ios_char_setstate
        }
        "?uncaught_exception@std@@YA_NXZ" => msvc_uncaught_exception,
        "_Thrd_hardware_concurrency" => msvc_thrd_hardware_concurrency,
        "_Thrd_yield" => msvc_thrd_yield,
        "??0_Lockit@std@@QEAA@H@Z" => msvc_lockit_ctor,
        "??1_Lockit@std@@QEAA@XZ" => msvc_lockit_dtor,
        "_Mtx_lock" => msvc_mtx_lock,
        "_Mtx_unlock" => msvc_mtx_unlock,
        "_Cnd_init" => msvc_cnd_init,
        "_Cnd_destroy" => msvc_cnd_destroy,
        "_Cnd_signal" => msvc_cnd_signal,
        "_Cnd_broadcast" => msvc_cnd_broadcast,
        "_Cnd_wait" => msvc_cnd_wait,
        "??0?$basic_streambuf@DU?$char_traits@D@std@@@std@@IEAA@XZ" => {
            msvc_basic_streambuf_char_default
        }
        "??0?$basic_iostream@DU?$char_traits@D@std@@@std@@QEAA@PEAV?$basic_streambuf@DU?$char_traits@D@std@@@1@@Z" => {
            msvc_basic_iostream_char_default
        }
        "??0?$basic_streambuf@_WU?$char_traits@_W@std@@@std@@IEAA@XZ" => {
            msvc_basic_streambuf_wchar_default
        }
        "??0?$basic_iostream@_WU?$char_traits@_W@std@@@std@@QEAA@PEAV?$basic_streambuf@_WU?$char_traits@_W@std@@@1@@Z" => {
            msvc_basic_iostream_wchar_default
        }
        "??0?$basic_ios@DU?$char_traits@D@std@@@std@@IEAA@XZ" => |p| {
            msvc_basic_ios_default(
                p,
                "msvcp:basic_ios:char:vtable",
                "??1?$basic_ios@DU?$char_traits@D@std@@@std@@UEAA@XZ",
                false,
            )
        },
        "??0?$basic_ios@GU?$char_traits@G@std@@@std@@IEAA@XZ" => |p| {
            msvc_basic_ios_default(
                p,
                "msvcp:basic_ios:char16:vtable",
                "??1?$basic_ios@GU?$char_traits@G@std@@@std@@UEAA@XZ",
                true,
            )
        },
        "??0?$basic_ios@_WU?$char_traits@_W@std@@@std@@IEAA@XZ" => |p| {
            msvc_basic_ios_default(
                p,
                "msvcp:basic_ios:wchar:vtable",
                "??1?$basic_ios@_WU?$char_traits@_W@std@@@std@@UEAA@XZ",
                true,
            )
        },
        // ── startup / process ──
        "_fpreset" => fpreset,
        "_configure_narrow_argv" => |p| {
            ensure_args(p)?;
            Ok(0)
        },
        "_configure_wide_argv" => |p| {
            ensure_wide_args(p)?;
            Ok(0)
        },
        "__p__fmode" => |p| Ok(data_cell(p, "_fmode", 8)),
        "__p__commode" => |p| Ok(data_cell(p, "_commode", 8)),
        "__p___argc" => |p| {
            ensure_args(p)?;
            let a = data_cell(p, "__argc", 8);
            p.wr32(a, p.extra.argc)?;
            Ok(a)
        },
        "__p___argv" => |p| {
            ensure_args(p)?;
            let a = data_cell(p, "__argv", 8);
            p.wrptr(a, p.extra.argv)?;
            Ok(a)
        },
        "__p___wargv" => |p| {
            ensure_wide_args(p)?;
            let a = data_cell(p, "__wargv", 8);
            p.wrptr(a, p.extra.wargv)?;
            Ok(a)
        },
        "__p__environ" => |p| {
            let environ = ensure_environment(p, false)?;
            pointer_data_cell(p, "_environ", environ)
        },
        "__p__wenviron" => |p| {
            let environ = ensure_environment(p, true)?;
            pointer_data_cell(p, "_wenviron", environ)
        },
        "__p__acmdln" => |p| {
            let command_line = kernel32::command_line_a(p)?;
            pointer_data_cell(p, "_acmdln", command_line)
        },
        "__p__wcmdln" => |p| {
            let command_line = kernel32::command_line_w(p)?;
            pointer_data_cell(p, "_wcmdln", command_line)
        },
        "__p__pgmptr" => |p| {
            let program = ensure_program_name(p, false)?;
            pointer_data_cell(p, "_pgmptr", program)
        },
        "__p__wpgmptr" => |p| {
            let program = ensure_program_name(p, true)?;
            pointer_data_cell(p, "_wpgmptr", program)
        },
        "_get_initial_narrow_environment" => |p| ensure_initial_environment(p, false),
        "_get_initial_wide_environment" => |p| ensure_initial_environment(p, true),
        "__getmainargs" => |p| crt_main_args(p, false),
        "__wgetmainargs" => |p| crt_main_args(p, true),
        "_get_narrow_winmain_command_line" => |p| kernel32::command_line_a(p),
        "_initterm" => |p| initterm(p, false),
        "_initterm_e" => |p| initterm(p, true),
        "atexit" | "_crt_atexit" | "_onexit" => |p| {
            let f = p.arg(0);
            p.atexit.push(f);
            Ok(if p.arg(0) != 0 { 0 } else { u64::MAX })
        },
        "_initialize_onexit_table" => |p| {
            initialize_onexit_table(p)
        },
        "_register_onexit_function" => register_onexit_function,
        "_execute_onexit_table" => execute_onexit_table,
        "exit" | "_exit" | "quick_exit" | "_Exit" => |p| {
            let c = p.arg(0) as u32;
            p.finish_exit(c).map(|_| 0)
        },
        "_cexit" | "_c_exit" => |p| {
            while let Some(f) = p.atexit.pop() {
                p.call_guest(f, &[])?;
            }
            Ok(0)
        },
        "abort" | "_abort" => crt_abort,
        "_amsg_exit" | "_wassert" | "_assert" => |p| {
            let msg = if p.arg(0) > 0x1000 { p.read_cstr(p.arg(0)).unwrap_or_default() } else { String::new() };
            p.stderr.extend_from_slice(format!("assertion/runtime error {msg}\n").as_bytes());
            p.finish_exit(3).map(|_| 0)
        },
        "_errno" => |p| Ok(errno_cell(p)),
        "__doserrno" => |p| Ok(errno_cell(p) + 4),
        "_lock_locales" => lock_locales,
        "_unlock_locales" => unlock_locales,
        "_lock" => crt_lock,
        "_unlock" => crt_unlock,
        "_lock_file" => crt_lock_file,
        "_unlock_file" => crt_unlock_file,
        "_mlock" => crt_mlock,
        "_munlock" => crt_munlock,
        "_getpid" => |_| Ok(0x1000),
        "___lc_codepage_func" | "__lc_codepage_func" => |_| Ok(1252),
        "___lc_locale_name_func" => crt_locale_name,
        "localeconv" => |p| {
            if p.extra.locale_cell == 0 {
                let lc = kernel32::alloc_block(p, 128, true);
                let dot = kernel32::alloc_block(p, 2, true);
                p.write_cstr(dot, ".")?;
                let empty = kernel32::alloc_block(p, 2, true);
                p.wr64(lc, dot)?; // decimal_point
                for i in 1..4 {
                    p.wr64(lc + 8 * i, empty)?; // thousands_sep, grouping, int_curr_symbol
                }
                for i in 4..10 {
                    p.wr64(lc + 8 * i, empty)?;
                }
                p.extra.locale_cell = lc;
            }
            Ok(p.extra.locale_cell)
        },
        "setlocale" => setlocale,
        "_wsetlocale" => wsetlocale,
        "strerror" | "_strerror" => |p| {
            let e = p.arg(0) as i32;
            let msg = match e {
                0 => "No error",
                2 => "No such file or directory",
                12 => "Not enough space",
                13 => "Permission denied",
                22 => "Invalid argument",
                34 => "Result too large",
                _ => "Unknown error",
            };
            let a = kernel32::alloc_block(p, msg.len() as u64 + 1, true);
            p.write_cstr(a, msg)?;
            Ok(a)
        },
        "signal" => crt_signal,
        "raise" => crt_raise,
        "_XcptFilter" => crt_xcpt_filter,
        "_except_handler3" => crate::exception32::except_handler3,
        "_abnormal_termination" | "__abnormal_termination" => {
            |p| Ok(p.seh_abnormal_termination as u64)
        }
        "__C_specific_handler" => crate::exception::c_specific_handler,
        "_except_handler4_common" => crate::exception32::except_handler4_common,
        "__CxxFrameHandler3" => crate::msvc_eh::frame_handler3,
        "__CxxFrameHandler4" => crate::msvc_eh::frame_handler4,
        "_CxxThrowException" => crate::msvc_eh::cxx_throw_exception,
        "__std_terminate" | "terminate" => crate::msvc_eh::std_terminate,
        "__std_exception_copy" => msvc_std_exception_copy,
        "__std_exception_destroy" => msvc_std_exception_destroy,
        "_purecall" => msvc_purecall,
        "_get_purecall_handler" => |p| Ok(p.extra.purecall_handler),
        "_set_purecall_handler" => |p| {
            let previous = p.extra.purecall_handler;
            p.extra.purecall_handler = p.arg(0);
            Ok(previous)
        },
        "_get_invalid_parameter_handler" => |p| Ok(p.extra.invalid_parameter_handler),
        "_set_invalid_parameter_handler" => |p| {
            let previous = p.extra.invalid_parameter_handler;
            p.extra.invalid_parameter_handler = p.arg(0);
            Ok(previous)
        },
        "_set_abort_behavior" => |p| {
            let flags = p.arg(0) as u32;
            let mask = p.arg(1) as u32;
            let previous = p
                .extra
                .abort_behavior_flags
                .unwrap_or(CRT_ABORT_DEFAULT_BEHAVIOR);
            p.extra.abort_behavior_flags = Some((previous & !mask) | (flags & mask));
            Ok(previous as u64)
        },
        "__current_exception" => |p| crate::msvc_eh::current_cpp_exception_cell(p, false),
        "__current_exception_context" => |p| crate::msvc_eh::current_cpp_exception_cell(p, true),
        "_setjmp3" => setjmp3,
        "_setjmp" | "_setjmpex" | "setjmp" | "__intrinsic_setjmp" => setjmp,
        "longjmp" => longjmp,
        "_Query_perf_counter" => msvc_query_perf_counter,
        "_Query_perf_frequency" => |_| Ok(kernel32::QUERY_PERFORMANCE_FREQUENCY),
        "_beginthreadex" => kernel32::create_thread,
        "_beginthread" => |p| Err(Stop::Unimplemented { dll: "crt".into(), name: "_beginthread (cdecl thread wrapper is not implemented yet)".into() }.record(p)),
        // Pointer encoding only obfuscates an in-process value. The guest and HLEs share one
        // isolated address space, so preserving the pointer is equivalent for encode/decode pairs.
        "_encode_pointer" | "_decode_pointer" => |p| Ok(p.arg(0)),
        // ── memory ──
        "malloc" => |p| {
            let n = p.arg(0);
            Ok(kernel32::alloc_block(p, n, false))
        },
        "calloc" => |p| {
            let n = p.arg(0).wrapping_mul(p.arg(1));
            Ok(kernel32::alloc_block(p, n, true))
        },
        "realloc" => |p| {
            let (a, n) = (p.arg(0), p.arg(1));
            kernel32::realloc_block(p, a, n, 0)
        },
        "free" => |p| {
            let a = p.arg(0);
            if a != 0 {
                kernel32::free_block(p, a);
            }
            Ok(0)
        },
        "_msize" => |p| Ok(p.heap.size_of(p.arg(0)).unwrap_or(0)),
        "_aligned_malloc" => |p| {
            let (n, al) = (p.arg(0), p.arg(1).max(16));
            let raw = kernel32::alloc_block(p, n + al + 16, false);
            if raw == 0 {
                return Ok(0);
            }
            let a = (raw + 8 + al) & !(al - 1);
            p.wr64(a - 8, raw)?;
            Ok(a)
        },
        "_aligned_free" => |p| {
            let a = p.arg(0);
            if a != 0 {
                let raw = p.rd64(a - 8)?;
                kernel32::free_block(p, raw);
            }
            Ok(0)
        },
        "memcpy" | "memmove" => |p| {
            let (d, s, n) = (p.arg(0), p.arg(1), p.arg(2) as usize);
            let b = p.read_bytes(s, n)?;
            p.write_bytes(d, &b)?;
            Ok(d)
        },
        "memcpy_s" | "memmove_s" => |p| {
            let (d, dn, s, n) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3) as usize);
            if n as u64 > dn {
                return Ok(34);
            }
            let b = p.read_bytes(s, n)?;
            p.write_bytes(d, &b)?;
            Ok(0)
        },
        "memset" => |p| {
            let (d, v, n) = (p.arg(0), p.arg(1) as u8, p.arg(2) as usize);
            p.write_bytes(d, &vec![v; n])?;
            Ok(d)
        },
        "memcmp" | "_memicmp" => |p| {
            let (a, b, n) = (p.arg(0), p.arg(1), p.arg(2) as usize);
            let (x, y) = (p.read_bytes(a, n)?, p.read_bytes(b, n)?);
            Ok(x.cmp(&y) as i32 as i64 as u64)
        },
        "memchr" => |p| {
            let (a, c, n) = (p.arg(0), p.arg(1) as u8, p.arg(2) as usize);
            let x = p.read_bytes(a, n)?;
            Ok(x.iter().position(|&b| b == c).map(|i| a + i as u64).unwrap_or(0))
        },
        // ── strings ──
        "strlen" => |p| Ok(p.mem.read_cstr(p.arg(0), 1 << 30).map_err(|e| Stop::Crash(format!("{e:?}")))?.len() as u64),
        "strnlen" => |p| {
            let (a, m) = (p.arg(0), p.arg(1) as usize);
            Ok(p.mem.read_cstr(a, m).map_err(|e| Stop::Crash(format!("{e:?}")))?.len() as u64)
        },
        "strcmp" | "strncmp" | "_stricmp" | "_strnicmp" | "stricmp" | "strcmpi" | "_strcmpi" | "strncasecmp" | "strcasecmp" => strcmp_family,
        "strcpy" => |p| {
            let (d, s) = (p.arg(0), p.read_cstr_raw(p.arg(1))?);
            p.write_bytes(d, &[s.as_slice(), &[0]].concat())?;
            Ok(d)
        },
        "strcpy_s" => |p| {
            let (d, n, s) = (p.arg(0), p.arg(1) as usize, p.read_cstr_raw(p.arg(2))?);
            if s.len() + 1 > n {
                return Ok(34);
            }
            p.write_bytes(d, &[s.as_slice(), &[0]].concat())?;
            Ok(0)
        },
        "strncpy" => |p| {
            let (d, s, n) = (p.arg(0), p.read_cstr_raw(p.arg(1))?, p.arg(2) as usize);
            let mut v = s.clone();
            v.truncate(n);
            v.resize(n, 0);
            p.write_bytes(d, &v)?;
            Ok(d)
        },
        "strcat" => |p| {
            let (d, s) = (p.arg(0), p.read_cstr_raw(p.arg(1))?);
            let dl = p.read_cstr_raw(d)?.len() as u64;
            p.write_bytes(d + dl, &[s.as_slice(), &[0]].concat())?;
            Ok(d)
        },
        "strcat_s" => |p| {
            let (d, n, s) = (p.arg(0), p.arg(1) as usize, p.read_cstr_raw(p.arg(2))?);
            let dl = p.read_cstr_raw(d)?.len();
            if dl + s.len() + 1 > n {
                return Ok(34);
            }
            p.write_bytes(d + dl as u64, &[s.as_slice(), &[0]].concat())?;
            Ok(0)
        },
        "strncat" => |p| {
            let (d, s, n) = (p.arg(0), p.read_cstr_raw(p.arg(1))?, p.arg(2) as usize);
            let dl = p.read_cstr_raw(d)?.len() as u64;
            let take = &s[..s.len().min(n)];
            p.write_bytes(d + dl, &[take, &[0]].concat())?;
            Ok(d)
        },
        "strchr" => |p| {
            let (a, c) = (p.arg(0), p.arg(1) as u8);
            let s = p.read_cstr_raw(a)?;
            if c == 0 {
                return Ok(a + s.len() as u64);
            }
            Ok(s.iter().position(|&b| b == c).map(|i| a + i as u64).unwrap_or(0))
        },
        "strrchr" => |p| {
            let (a, c) = (p.arg(0), p.arg(1) as u8);
            let s = p.read_cstr_raw(a)?;
            if c == 0 {
                return Ok(a + s.len() as u64);
            }
            Ok(s.iter().rposition(|&b| b == c).map(|i| a + i as u64).unwrap_or(0))
        },
        "strstr" => |p| {
            let (a, n) = (p.arg(0), p.read_cstr_raw(p.arg(1))?);
            let s = p.read_cstr_raw(a)?;
            if n.is_empty() {
                return Ok(a);
            }
            Ok(s.windows(n.len()).position(|w| w == n.as_slice()).map(|i| a + i as u64).unwrap_or(0))
        },
        "strspn" => |p| {
            Err(Stop::Unimplemented { dll: "crt".into(), name: "strspn".into() }.record(p))
        },
        "strcspn" => |p| {
            let (string, reject) = (p.read_cstr_raw(p.arg(0))?, p.read_cstr_raw(p.arg(1))?);
            let rejected = c_string_byte_set(&reject);
            Ok(string.iter().take_while(|&&byte| !rejected[byte as usize]).count() as u64)
        },
        "strpbrk" => |p| {
            let (string_address, string, accept) = (
                p.arg(0),
                p.read_cstr_raw(p.arg(0))?,
                p.read_cstr_raw(p.arg(1))?,
            );
            let accepted = c_string_byte_set(&accept);
            Ok(string
                .iter()
                .position(|&byte| accepted[byte as usize])
                .map(|offset| string_address + offset as u64)
                .unwrap_or(0))
        },
        "_strdup" | "strdup" => |p| {
            let s = p.read_cstr_raw(p.arg(0))?;
            let a = kernel32::alloc_block(p, s.len() as u64 + 1, true);
            p.write_bytes(a, &[s.as_slice(), &[0]].concat())?;
            Ok(a)
        },
        "strtok" => |p| {
            let (s, delim) = (p.arg(0), p.read_cstr_raw(p.arg(1))?);
            let mut cur = if s != 0 { s } else { p.extra.strtok_ptr };
            if cur == 0 {
                return Ok(0);
            }
            loop {
                let c = p.rd8(cur)?;
                if c == 0 {
                    p.extra.strtok_ptr = 0;
                    return Ok(0);
                }
                if !delim.contains(&c) {
                    break;
                }
                cur += 1;
            }
            let start = cur;
            loop {
                let c = p.rd8(cur)?;
                if c == 0 {
                    p.extra.strtok_ptr = 0;
                    break;
                }
                if delim.contains(&c) {
                    p.wr8(cur, 0)?;
                    p.extra.strtok_ptr = cur + 1;
                    break;
                }
                cur += 1;
            }
            Ok(start)
        },
        "toupper" | "_toupper" => |p| Ok((p.arg(0) as u8).to_ascii_uppercase() as u64),
        "tolower" | "_tolower" => |p| Ok((p.arg(0) as u8).to_ascii_lowercase() as u64),
        "towupper" => |p| Ok(char::from_u32(p.arg(0) as u32).map(|c| c.to_uppercase().next().unwrap() as u64).unwrap_or(p.arg(0))),
        "towlower" => |p| Ok(char::from_u32(p.arg(0) as u32).map(|c| c.to_lowercase().next().unwrap() as u64).unwrap_or(p.arg(0))),
        "isalpha" | "iswalpha" => |p| Ok((p.arg(0) < 256 && (p.arg(0) as u8).is_ascii_alphabetic()) as u64),
        "isdigit" | "iswdigit" => |p| Ok((p.arg(0) < 256 && (p.arg(0) as u8).is_ascii_digit()) as u64),
        "isalnum" => |p| Ok((p.arg(0) < 256 && (p.arg(0) as u8).is_ascii_alphanumeric()) as u64),
        "isspace" | "iswspace" => |p| Ok((p.arg(0) < 256 && matches!(p.arg(0) as u8, b' ' | b'\t'..=b'\r')) as u64),
        "isupper" => |p| Ok((p.arg(0) < 256 && (p.arg(0) as u8).is_ascii_uppercase()) as u64),
        "islower" => |p| Ok((p.arg(0) < 256 && (p.arg(0) as u8).is_ascii_lowercase()) as u64),
        "ispunct" => |p| Ok((p.arg(0) < 256 && (p.arg(0) as u8).is_ascii_punctuation()) as u64),
        "isxdigit" | "iswxdigit" => |p| Ok((p.arg(0) < 256 && (p.arg(0) as u8).is_ascii_hexdigit()) as u64),
        "isprint" => |p| Ok((p.arg(0) < 256 && ((p.arg(0) as u8).is_ascii_graphic() || p.arg(0) == 32)) as u64),
        "iscntrl" => |p| Ok((p.arg(0) < 256 && (p.arg(0) as u8).is_ascii_control()) as u64),
        "isgraph" => |p| Ok((p.arg(0) < 256 && (p.arg(0) as u8).is_ascii_graphic()) as u64),
        "wcslen" => |p| Ok(p.read_wstr(p.arg(0))?.encode_utf16().count() as u64),
        "__pctype_func" => |p| Ok(ctype_table(p)),
        "___mb_cur_max_func" | "__mb_cur_max_func" => |_| Ok(1),
        // ── numbers ──
        "abs" => |p| Ok((p.arg(0) as i32).unsigned_abs() as u64),
        "labs" | "llabs" | "_abs64" => |p| Ok((p.arg(0) as i64).unsigned_abs()),
        "atoi" | "atol" => |p| Ok(parse_int(&p.read_cstr(p.arg(0))?, 10).0 as i32 as u32 as u64),
        "atoll" | "_atoi64" => |p| Ok(parse_int(&p.read_cstr(p.arg(0))?, 10).0 as u64),
        "atof" | "_atof_l" => |p| {
            let s = p.read_cstr(p.arg(0))?;
            p.set_retf(parse_float(&s).0);
            Ok(0)
        },
        "_atoflt" => crt_atoflt,
        "strtol" | "strtoul" | "strtoll" | "strtoull" | "_strtoi64" | "_strtoui64" => strtol_family,
        "strtod" => |p| {
            let (s, end) = (p.arg(0), p.arg(1));
            let st = p.read_cstr(s)?;
            let (v, used) = parse_float(&st);
            if end != 0 {
                p.wr64(end, s + used as u64)?;
            }
            p.set_retf(v);
            Ok(0)
        },
        "_itoa" | "itoa" | "_ltoa" | "_ultoa" | "_i64toa" | "_ui64toa" | "_itoa_s" => itoa_family,
        "rand" => |p| {
            p.extra.rand_state = p.extra.rand_state.wrapping_mul(214013).wrapping_add(2531011);
            Ok(((p.extra.rand_state >> 16) & 0x7fff) as u64)
        },
        "srand" => |p| {
            p.extra.rand_state = p.arg(0) as u32;
            Ok(0)
        },
        "qsort" => qsort,
        "bsearch" => bsearch,
        // ── time ──
        "time" | "_time64" | "_time32" => |p| {
            let t = 1_767_225_600 + (p.cpu.icount + p.extra.sleep_adjust) / 100_000_000;
            let out = p.arg(0);
            if out != 0 {
                p.wr64(out, t)?;
            }
            Ok(t)
        },
        "localtime64" | "_localtime64" => localtime64,
        "_gmtime64" => gmtime64,
        "_ctime64" => ctime64,
        "asctime" => asctime,
        "_difftime64" => difftime64,
        "_mktime64" => mktime64,
        "_wctime64_s" => wctime64_s,
        "strftime" => strftime,
        "clock" => |p| Ok((p.cpu.icount + p.extra.sleep_adjust) / 100_000),
        // ── math ──
        "sqrt" | "sin" | "cos" | "tan" | "atan" | "asin" | "acos" | "sinh" | "cosh" | "tanh" | "exp" | "log" | "log10" | "log2" | "floor" | "ceil" | "fabs" | "round" | "trunc" | "cbrt" | "exp2" | "expm1" | "log1p" => math1,
        "pow" | "atan2" | "fmod" | "hypot" | "fmin" | "fmax" | "copysign" => math2,
        "sqrtf" | "sinf" | "cosf" | "tanf" | "atanf" | "asinf" | "acosf" | "expf" | "logf" | "log10f" | "log2f" | "floorf" | "ceilf" | "fabsf" | "roundf" | "truncf" | "sinhf" | "coshf" | "tanhf" => math1f,
        "powf" | "atan2f" | "fmodf" | "hypotf" | "fminf" | "fmaxf" => math2f,
        "ldexp" => |p| {
            let (x, e) = (p.argf(0), p.arg(1) as i32);
            p.set_retf(x * 2f64.powi(e));
            Ok(0)
        },
        "frexp" => |p| {
            let (x, pe) = (p.argf(0), p.arg(1));
            let (m, e) = frexp(x);
            p.wr32(pe, e as u32)?;
            p.set_retf(m);
            Ok(0)
        },
        "modf" => |p| {
            let (x, pi) = (p.argf(0), p.arg(1));
            p.wr64(pi, x.trunc().to_bits())?;
            p.set_retf(x - x.trunc());
            Ok(0)
        },
        "_finite" | "_isnan" | "_fpclassify" => |p| {
            let x = p.argf(0);
            Ok(match () {
                _ => x.is_finite() as u64,
            })
        },
        // ── stdio ──
        "__acrt_iob_func" => |p| {
            let i = p.arg(0);
            let base = ensure_iob(p)?;
            Ok(base + i * FILE_SIZE)
        },
        "_get_stream_buffer_pointers" => get_stream_buffer_pointers,
        "__iob_func" => |p| ensure_iob(p),
        "puts" => |p| {
            let mut s = p.mem.read_cstr(p.arg(0), 1 << 20).map_err(|e| Stop::Crash(format!("{e:?}")))?;
            s.push(b'\n');
            p.stdout.extend_from_slice(&s);
            Ok(1)
        },
        "putchar" | "_putch" => |p| {
            let c = p.arg(0) as u8;
            p.stdout.push(c);
            Ok(c as u64)
        },
        "fputs" => |p| {
            let (s, fp) = (p.read_cstr_raw(p.arg(0))?, p.arg(1));
            stream_write(p, fp, &s)?;
            Ok(1)
        },
        "fputc" | "putc" | "_fputc_nolock" | "_putc_nolock" => |p| {
            let (c, fp) = (p.arg(0) as u8, p.arg(1));
            stream_write(p, fp, &[c])?;
            Ok(c as u64)
        },
        "fwrite" | "_fwrite_nolock" => |p| {
            let (buf, sz, n, fp) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
            let data = p.read_bytes(buf, (sz * n) as usize)?;
            let w = stream_write(p, fp, &data)?;
            Ok(if sz == 0 { 0 } else { w as u64 / sz })
        },
        "fread" | "_fread_nolock" => |p| {
            let (buf, sz, n, fp) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
            let requested = sz
                .checked_mul(n)
                .and_then(|count| usize::try_from(count).ok())
                .ok_or_else(|| Stop::Crash("fread byte count overflow".into()))?;
            let data = stream_read(p, fp, requested)?;
            p.write_bytes(buf, &data)?;
            Ok(if sz == 0 { 0 } else { data.len() as u64 / sz })
        },
        "fgetc" | "getc" | "_fgetc_nolock" => |p| {
            let fp = p.arg(0);
            Ok(stream_read_byte(p, fp)?.map(|b| b as u64).unwrap_or(0xffff_ffff))
        },
        "getchar" => |p| {
            let fp = ensure_iob(p)?;
            Ok(stream_read_byte(p, fp)?.map(|b| b as u64).unwrap_or(0xffff_ffff))
        },
        "ungetc" => |p| {
            let (c, fp) = (p.arg(0) as u8, p.arg(1));
            if let Some(f) = p.extra.files.get_mut(&fp) {
                f.ungot = Some(c);
            }
            Ok(c as u64)
        },
        "fgets" => |p| {
            let (buf, n, fp) = (p.arg(0), p.arg(1) as usize, p.arg(2));
            let mut v = Vec::new();
            while v.len() + 1 < n {
                match stream_read_byte(p, fp)? {
                    Some(b) => {
                        v.push(b);
                        if b == b'\n' {
                            break;
                        }
                    }
                    None => break,
                }
            }
            if v.is_empty() {
                return Ok(0);
            }
            v.push(0);
            p.write_bytes(buf, &v)?;
            Ok(buf)
        },
        "fopen" => |p| {
            let (name, mode) = (p.read_cstr(p.arg(0))?, p.read_cstr(p.arg(1))?);
            fopen_impl(p, &name, &mode)
        },
        "_wfopen" => |p| {
            let (name, mode) = (p.read_wstr(p.arg(0))?, p.read_wstr(p.arg(1))?);
            fopen_impl(p, &name, &mode)
        },
        "fopen_s" => |p| {
            let (out, name, mode) = (p.arg(0), p.read_cstr(p.arg(1))?, p.read_cstr(p.arg(2))?);
            let fp = fopen_impl(p, &name, &mode)?;
            p.wr64(out, fp)?;
            Ok(if fp == 0 { 2 } else { 0 })
        },
        "_wfopen_s" => |p| {
            let (out, name, mode) = (p.arg(0), p.read_wstr(p.arg(1))?, p.read_wstr(p.arg(2))?);
            let fp = fopen_impl(p, &name, &mode)?;
            p.wr64(out, fp)?;
            Ok(if fp == 0 { 2 } else { 0 })
        },
        "fclose" => |p| {
            let fp = p.arg(0);
            Ok(p.extra.files.remove(&fp).is_some() as u64 ^ 1)
        },
        "fflush" | "_fflush_nolock" => fflush,
        "_flushall" => flush_all,
        "feof" => |p| Ok(p.extra.files.get(&p.arg(0)).map(|f| f.eof as u64).unwrap_or(1)),
        "ferror" => |p| Ok(p.extra.files.get(&p.arg(0)).map(|f| f.err as u64).unwrap_or(1)),
        "clearerr" => |p| {
            if let Some(f) = p.extra.files.get_mut(&p.arg(0)) {
                f.eof = false;
                f.err = false;
            }
            Ok(0)
        },
        "fseek" | "_fseeki64" => |p| {
            let (fp, off, whence) = (p.arg(0), p.arg(1) as i64, p.arg(2));
            let len = match p.extra.files.get(&fp) {
                Some(CFile { kind: FileKind::Vfs { path, .. }, .. }) => p.vfs.file_len(path).unwrap_or(0),
                _ => return Ok(u64::MAX),
            };
            if let Some(CFile { kind: FileKind::Vfs { pos, .. }, eof, .. }) = p.extra.files.get_mut(&fp) {
                let base = match whence {
                    0 => 0i64,
                    1 => *pos as i64,
                    _ => len as i64,
                };
                *pos = (base + off).max(0) as usize;
                *eof = false;
            }
            Ok(0)
        },
        "ftell" | "_ftelli64" => |p| match p.extra.files.get(&p.arg(0)) {
            Some(CFile { kind: FileKind::Vfs { pos, .. }, .. }) => Ok(*pos as u64),
            _ => Ok(u64::MAX),
        },
        "rewind" => |p| {
            if let Some(CFile { kind: FileKind::Vfs { pos, .. }, eof, .. }) = p.extra.files.get_mut(&p.arg(0)) {
                *pos = 0;
                *eof = false;
            }
            Ok(0)
        },
        "remove" | "_unlink" => |p| {
            let n = p.read_cstr(p.arg(0))?;
            crt_unlink_path(p, &n)
        },
        "_wunlink" => crt_wunlink,
        "printf" | "_printf" => |p| {
            let out = do_printf(p, p.arg(0), &mut SlotArgs(1), false)?;
            p.stdout.extend_from_slice(&out);
            Ok(out.len() as u64)
        },
        "fprintf" => |p| {
            let fp = p.arg(0);
            let out = do_printf(p, p.arg(1), &mut SlotArgs(2), false)?;
            stream_write(p, fp, &out)?;
            Ok(out.len() as u64)
        },
        "sprintf" | "_sprintf" => |p| {
            let buf = p.arg(0);
            let out = do_printf(p, p.arg(1), &mut SlotArgs(2), false)?;
            store_formatted(p, buf, None, &out, false)?;
            Ok(out.len() as u64)
        },
        "snprintf" | "_snprintf" => |p| {
            let (buf, n) = (p.arg(0), p.arg(1));
            let out = do_printf(p, p.arg(2), &mut SlotArgs(3), false)?;
            store_formatted(p, buf, Some(n), &out, false)?;
            Ok(out.len() as u64)
        },
        "_snprintf_s" => |p| crt_secure_vsnprintf(p, false, false),
        "_vsnprintf_s" => |p| crt_secure_vsnprintf(p, false, true),
        "_vswprintf_c_l" => |p| crt_secure_vsnprintf(p, true, true),
        "vprintf" => |p| {
            let out = do_printf(p, p.arg(0), &mut VaList(p.arg(1)), false)?;
            p.stdout.extend_from_slice(&out);
            Ok(out.len() as u64)
        },
        "vfprintf" => |p| {
            let fp = p.arg(0);
            let out = do_printf(p, p.arg(1), &mut VaList(p.arg(2)), false)?;
            stream_write(p, fp, &out)?;
            Ok(out.len() as u64)
        },
        "vsprintf" => |p| {
            let buf = p.arg(0);
            let out = do_printf(p, p.arg(1), &mut VaList(p.arg(2)), false)?;
            store_formatted(p, buf, None, &out, false)?;
            Ok(out.len() as u64)
        },
        "vsnprintf" | "_vsnprintf" => |p| {
            let (buf, n) = (p.arg(0), p.arg(1));
            let out = do_printf(p, p.arg(2), &mut VaList(p.arg(3)), false)?;
            store_formatted(p, buf, Some(n), &out, false)?;
            Ok(out.len() as u64)
        },
        "__stdio_common_vfprintf" | "__stdio_common_vfwprintf" => |p| {
            let wide = false;
            let (fp, fmt, va) = (p.arg(1), p.arg(2), p.arg(4));
            let out = do_printf(p, fmt, &mut VaList(va), wide)?;
            stream_write(p, fp, &out)?;
            Ok(out.len() as u64)
        },
        "__stdio_common_vsprintf" | "__stdio_common_vsnprintf_s" | "__stdio_common_vsprintf_s" => |p| {
            // vsprintf: (opts, buf, len, fmt, locale, va); vsnprintf_s: (opts, buf, len, max, fmt, locale, va)
            let snp = p.thunks_is_snprintf_s();
            let (buf, len) = (p.arg(1), p.arg(2));
            let (fmt, va) = if snp { (p.arg(4), p.arg(6)) } else { (p.arg(3), p.arg(5)) };
            let out = do_printf(p, fmt, &mut VaList(va), false)?;
            store_formatted(p, buf, Some(len), &out, false)?;
            Ok(out.len() as u64)
        },
        "__stdio_common_vswprintf" | "__stdio_common_vswprintf_s" => |p| {
            let (buf, len, fmt, va) = (p.arg(1), p.arg(2), p.arg(3), p.arg(5));
            let out = do_printf(p, fmt, &mut VaList(va), true)?;
            store_formatted(p, buf, Some(len), &out, true)?;
            Ok(len_for(&out, true))
        },
        "sscanf" | "__stdio_common_vsscanf" => sscanf,
        "vsscanf" => vsscanf,
        "getenv" | "_wgetenv" => |p| {
            let k = p.read_cstr(p.arg(0))?;
            match p.env.iter().find(|(a, _)| a.eq_ignore_ascii_case(&k)).map(|(_, v)| v.clone()) {
                Some(v) => {
                    let a = kernel32::alloc_block(p, v.len() as u64 + 1, true);
                    p.write_cstr(a, &v)?;
                    Ok(a)
                }
                None => Ok(0),
            }
        },
        "_putenv" => putenv,
        "_fileno" => crt_fileno,
        "_isatty" => |p| Ok((p.arg(0) < 3) as u64),
        "_write" => crt_write,
        _ => return None,
    })
}

const LOCALE_LOCK_ADDRESS: u64 = 0xffff_ffff_ffff_ff00;

fn lock_locales(p: &mut Process) -> R {
    let id = p.current_thread_id;
    if p.try_acquire_critical(LOCALE_LOCK_ADDRESS, id) {
        return Ok(0);
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "contended _lock_locales from a nested synchronous guest callback cannot be scheduled".into(),
        }
        .record(p));
    }
    let waiters = &mut p
        .critical_sections
        .entry(LOCALE_LOCK_ADDRESS)
        .or_default()
        .waiters;
    if !waiters.contains(&id) {
        waiters.push_back(id);
    }
    p.block_current_thread(ThreadWait::CriticalAcquire {
        address: LOCALE_LOCK_ADDRESS,
    });
    Ok(0)
}

fn unlock_locales(p: &mut Process) -> R {
    if !p.release_critical(LOCALE_LOCK_ADDRESS, p.current_thread_id) {
        return Err(Stop::Crash(
            "_unlock_locales called without owning the locale lock".into(),
        ));
    }
    p.refresh_waiters();
    Ok(0)
}

const CRT_LOCK_KEY_BASE: u64 = 0xffff_0000_0000_0000;
const CRT_FILE_LOCK_KEY_BASE: u64 = 0xfffe_0000_0000_0000;
const CRT_MLOCK_KEY_BASE: u64 = 0xfffd_0000_0000_0000;

fn crt_lock_address(base: u64, lock_id: u64) -> u64 {
    base | (lock_id as u32 as u64)
}

fn crt_file_lock_address(file: u64) -> u64 {
    CRT_FILE_LOCK_KEY_BASE | (file & 0x0000_ffff_ffff_ffff)
}

fn crt_lock_acquire(p: &mut Process, address: u64, lock_name: &str) -> Result<(), Stop> {
    let id = p.current_thread_id;
    if p.try_acquire_critical(address, id) {
        return Ok(());
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("contended {lock_name} from a nested synchronous guest callback"),
        }
        .record(p));
    }
    let waiters = &mut p.critical_sections.entry(address).or_default().waiters;
    if !waiters.contains(&id) {
        waiters.push_back(id);
    }
    p.block_current_thread(ThreadWait::CriticalAcquire { address });
    Ok(())
}

fn crt_lock_release(p: &mut Process, address: u64, lock_name: &str) -> R {
    if !p.release_critical(address, p.current_thread_id) {
        return Err(Stop::Crash(format!(
            "{lock_name} released by a thread that does not own it"
        )));
    }
    p.refresh_waiters();
    Ok(0)
}

fn crt_lock(p: &mut Process) -> R {
    crt_lock_acquire(p, crt_lock_address(CRT_LOCK_KEY_BASE, p.arg(0)), "_lock")?;
    Ok(0)
}

fn crt_unlock(p: &mut Process) -> R {
    crt_lock_release(p, crt_lock_address(CRT_LOCK_KEY_BASE, p.arg(0)), "_unlock")
}

fn crt_mlock(p: &mut Process) -> R {
    crt_lock_acquire(p, crt_lock_address(CRT_MLOCK_KEY_BASE, p.arg(0)), "_mlock")?;
    Ok(0)
}

fn crt_munlock(p: &mut Process) -> R {
    crt_lock_release(
        p,
        crt_lock_address(CRT_MLOCK_KEY_BASE, p.arg(0)),
        "_munlock",
    )
}

fn crt_lock_file(p: &mut Process) -> R {
    let file = p.arg(0);
    if !p.extra.files.contains_key(&file) {
        return Err(Stop::Crash(format!(
            "_lock_file received invalid FILE pointer {file:#x}"
        )));
    }
    crt_lock_acquire(p, crt_file_lock_address(file), "_lock_file")?;
    Ok(0)
}

fn crt_unlock_file(p: &mut Process) -> R {
    let file = p.arg(0);
    crt_lock_release(p, crt_file_lock_address(file), "_unlock_file")
}

fn crt_locale_name(p: &mut Process) -> R {
    // This runtime currently supports only the C locale. Keep one stable narrow string,
    // matching the locale-name pointer returned by the CRT's internal accessor.
    let key = "crt:locale-name:C";
    if let Some(&name) = p.extra.data_cells.get(key) {
        return Ok(name);
    }
    let name = kernel32::alloc_block(p, 2, true);
    p.write_cstr(name, "C")?;
    p.extra.data_cells.insert(key.to_string(), name);
    Ok(name)
}

fn fpreset(p: &mut Process) -> R {
    p.cpu.fpu_st = [0.0; 8];
    p.cpu.fpu_top = 0;
    p.cpu.fpu_cw = 0x037f;
    p.cpu.fpu_sw = 0;
    p.cpu.fpu_tw = 0xffff;
    Ok(0)
}

const FP_MCW_DN: u32 = 0x0300_0000;
const FP_MCW_EM: u32 = 0x0008_001f;
const FP_MCW_IC: u32 = 0x0004_0000;
const FP_MCW_RC: u32 = 0x0000_0300;
const FP_MCW_PC: u32 = 0x0003_0000;
const FP_EM_AMBIGUOUS: u32 = 0x8000_0000;
const FP_ERR_INVALID: u32 = 22;

const FP_EXCEPTION_BITS: [(u32, u8, u8); 6] = [
    (0x10, 0, 7),    // invalid operation
    (0x80000, 1, 8), // denormal operand
    (0x08, 2, 9),    // divide by zero
    (0x04, 3, 10),   // overflow
    (0x02, 4, 11),   // underflow
    (0x01, 5, 12),   // inexact / precision
];

fn fp_control_from_x87(control: u16) -> u32 {
    let mut result = 0;
    for (crt_bit, x87_bit, _) in FP_EXCEPTION_BITS {
        if control & (1 << x87_bit) != 0 {
            result |= crt_bit;
        }
    }
    result |= (((control >> 10) & 3) as u32) << 8;
    match control & 0x0300 {
        0x0000 => result |= 0x0002_0000, // 24-bit precision
        0x0200 => result |= 0x0001_0000, // 53-bit precision
        0x0300 => {}                     // 64-bit precision
        _ => result |= FP_EM_AMBIGUOUS,  // reserved x87 precision encoding
    }
    if control & 0x1000 != 0 {
        result |= FP_MCW_IC;
    }
    result
}

fn fp_control_from_mxcsr(control: u32) -> u32 {
    let mut result = 0;
    for (crt_bit, _, sse_bit) in FP_EXCEPTION_BITS {
        if control & (1 << sse_bit) != 0 {
            result |= crt_bit;
        }
    }
    result | (((control >> 13) & 3) << 8)
}

fn fp_control_word(p: &Process) -> u32 {
    let x87 = fp_control_from_x87(p.cpu.fpu_cw);
    let sse = fp_control_from_mxcsr(p.cpu.mxcsr);
    if (x87 ^ sse) & (FP_MCW_EM | FP_MCW_RC) != 0 {
        x87 | FP_EM_AMBIGUOUS
    } else {
        x87
    }
}

fn fp_update_control_word(
    p: &mut Process,
    new_control: u32,
    mask: u32,
    control_denormal_exception: bool,
) -> bool {
    if mask & !(FP_MCW_DN | FP_MCW_EM | FP_MCW_IC | FP_MCW_RC | FP_MCW_PC) != 0 {
        return false;
    }
    if mask & FP_MCW_PC != 0 && !matches!(new_control & FP_MCW_PC, 0 | 0x0001_0000 | 0x0002_0000) {
        return false;
    }

    let mut x87 = p.cpu.fpu_cw;
    let mut mxcsr = p.cpu.mxcsr;
    for (crt_bit, x87_bit, sse_bit) in FP_EXCEPTION_BITS {
        if mask & crt_bit == 0 || (crt_bit == 0x80000 && !control_denormal_exception) {
            continue;
        }
        let enabled = new_control & crt_bit != 0;
        if enabled {
            x87 |= 1 << x87_bit;
            mxcsr |= 1 << sse_bit;
        } else {
            x87 &= !(1 << x87_bit);
            mxcsr &= !(1 << sse_bit);
        }
    }
    if mask & FP_MCW_RC != 0 {
        let rounding = ((new_control & FP_MCW_RC) >> 8) as u16;
        x87 = (x87 & !0x0c00) | (rounding << 10);
        mxcsr = (mxcsr & !0x6000) | ((rounding as u32) << 13);
    }
    if mask & FP_MCW_PC != 0 {
        let precision = match new_control & FP_MCW_PC {
            0x0002_0000 => 0x0000, // 24-bit
            0x0001_0000 => 0x0200, // 53-bit
            _ => 0x0300,           // 64-bit
        };
        x87 = (x87 & !0x0300) | precision;
    }
    if mask & FP_MCW_IC != 0 {
        if new_control & FP_MCW_IC != 0 {
            x87 |= 0x1000;
        } else {
            x87 &= !0x1000;
        }
    }

    // Microsoft documents denormal value control as a no-op for x86. Its separate
    // denormal exception mask is handled by _control87, but not _controlfp*.
    p.cpu.fpu_cw = x87;
    p.cpu.mxcsr = mxcsr;
    true
}

fn controlfp_s(p: &mut Process) -> R {
    let (output, new_control, mask) = (p.arg(0), p.arg(1) as u32, p.arg(2) as u32);
    if !fp_update_control_word(p, new_control, mask, false) {
        let errno = errno_cell(p);
        p.wr32(errno, FP_ERR_INVALID)?;
        return Ok(FP_ERR_INVALID as u64);
    }
    if output != 0 {
        p.wr32(output, fp_control_word(p))?;
    }
    Ok(0)
}

fn controlfp(p: &mut Process, control_denormal_exception: bool) -> R {
    let (new_control, mask) = (p.arg(0) as u32, p.arg(1) as u32);
    let _ = fp_update_control_word(p, new_control, mask, control_denormal_exception);
    Ok(fp_control_word(p) as u64)
}

fn controlfp_compat(p: &mut Process) -> R {
    controlfp(p, false)
}

fn control87(p: &mut Process) -> R {
    controlfp(p, true)
}

fn fflush(p: &mut Process) -> R {
    let file = p.arg(0);
    ensure_iob(p)?;
    if file == 0 {
        for stream in p.extra.files.values_mut() {
            // Output is committed immediately by stream_write; input has only this one
            // pending byte to discard when the caller flushes all streams.
            stream.ungot = None;
        }
        return Ok(0);
    }
    let Some(stream) = p.extra.files.get_mut(&file) else {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(u64::MAX);
    };
    stream.ungot = None;
    Ok(0)
}

fn flush_all(p: &mut Process) -> R {
    ensure_iob(p)?;
    let count = p.extra.files.len() as u64;
    for stream in p.extra.files.values_mut() {
        stream.ungot = None;
    }
    Ok(count)
}

fn putenv(p: &mut Process) -> R {
    let assignment = p.read_cstr(p.arg(0))?;
    let Some((name, value)) = assignment.split_once('=') else {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(u64::MAX);
    };
    if name.is_empty() {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(u64::MAX);
    }

    if let Some(index) = p
        .env
        .iter()
        .position(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        if value.is_empty() {
            p.env.remove(index);
        } else {
            p.env[index].1 = value.to_string();
        }
    } else if !value.is_empty() {
        p.env.push((name.to_string(), value.to_string()));
    }
    refresh_environment(p)?;
    Ok(0)
}

const CRT_SIG_IGN: u64 = 1;
const CRT_SIG_ERR: u64 = u32::MAX as u64;
const CRT_VALID_SIGNALS: [i32; 6] = [2, 4, 8, 11, 15, 22];
const CRT_ABORT_WRITE_ABORT_MSG: u32 = 0x1;
const CRT_ABORT_CALL_REPORTFAULT: u32 = 0x2;
const CRT_ABORT_DEFAULT_BEHAVIOR: u32 = CRT_ABORT_WRITE_ABORT_MSG | CRT_ABORT_CALL_REPORTFAULT;
const CRT_FPE_DENORMAL: u32 = 0x82;
const CRT_FPE_ZERODIVIDE: u32 = 0x83;
const CRT_FPE_INEXACT: u32 = 0x86;
const CRT_FPE_INVALID: u32 = 0x81;
const CRT_FPE_OVERFLOW: u32 = 0x84;
const CRT_FPE_STACKOVERFLOW: u32 = 0x8a;
const CRT_FPE_UNDERFLOW: u32 = 0x85;

fn crt_xcpt_filter(p: &mut Process) -> R {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_XcptFilter for non-PE32 process".into(),
        }
        .record(p));
    }

    const STATUS_ACCESS_VIOLATION: u32 = 0xc000_0005;
    const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xc000_001d;
    const STATUS_PRIVILEGED_INSTRUCTION: u32 = 0xc000_0096;
    const STATUS_FLOAT_DENORMAL_OPERAND: u32 = 0xc000_008d;
    const STATUS_FLOAT_DIVIDE_BY_ZERO: u32 = 0xc000_008e;
    const STATUS_FLOAT_INEXACT_RESULT: u32 = 0xc000_008f;
    const STATUS_FLOAT_INVALID_OPERATION: u32 = 0xc000_0090;
    const STATUS_FLOAT_OVERFLOW: u32 = 0xc000_0091;
    const STATUS_FLOAT_STACK_CHECK: u32 = 0xc000_0092;
    const STATUS_FLOAT_UNDERFLOW: u32 = 0xc000_0093;

    let status = p.arg(0) as u32;
    let exception_pointers = p.arg(1) as u32 as u64;
    let mapped = match status {
        STATUS_ACCESS_VIOLATION => Some((11, None)), // SIGSEGV
        STATUS_ILLEGAL_INSTRUCTION | STATUS_PRIVILEGED_INSTRUCTION => Some((4, None)), // SIGILL
        STATUS_FLOAT_DENORMAL_OPERAND => Some((8, Some(CRT_FPE_DENORMAL))),
        STATUS_FLOAT_DIVIDE_BY_ZERO => Some((8, Some(CRT_FPE_ZERODIVIDE))),
        STATUS_FLOAT_INEXACT_RESULT => Some((8, Some(CRT_FPE_INEXACT))),
        STATUS_FLOAT_INVALID_OPERATION => Some((8, Some(CRT_FPE_INVALID))),
        STATUS_FLOAT_OVERFLOW => Some((8, Some(CRT_FPE_OVERFLOW))),
        STATUS_FLOAT_STACK_CHECK => Some((8, Some(CRT_FPE_STACKOVERFLOW))),
        STATUS_FLOAT_UNDERFLOW => Some((8, Some(CRT_FPE_UNDERFLOW))),
        _ => None,
    };

    let Some((signal, subcode)) = mapped else {
        return crate::exception32::unhandled_exception_filter_disposition(p, exception_pointers)
            .map(|disposition| disposition as u32 as u64);
    };

    let cell = crt_signal_cell(p, signal);
    let handler = p.rdptr(cell)?;
    match handler {
        0 => crate::exception32::unhandled_exception_filter_disposition(p, exception_pointers)
            .map(|disposition| disposition as u32 as u64),
        CRT_SIG_IGN => Ok(u32::MAX as u64), // EXCEPTION_CONTINUE_EXECUTION
        handler => {
            // MSVC resets non-FPE actions before entering their one-shot handler.
            if signal != 8 {
                p.wrptr(cell, 0)?;
            }
            if let Some(subcode) = subcode {
                p.call_guest(handler, &[signal as u64, subcode as u64])?;
            } else {
                p.call_guest(handler, &[signal as u64])?;
            }
            Ok(u32::MAX as u64) // EXCEPTION_CONTINUE_EXECUTION
        }
    }
}

fn crt_signal_cell(p: &mut Process, signal: i32) -> u64 {
    let width = guest_pointer_width(p);
    data_cell(p, &format!("crt:signal:{signal}"), width)
}

fn crt_signal(p: &mut Process) -> R {
    let signal = p.arg(0) as u32 as i32;
    let handler = if guest_pointer_width(p) == 4 {
        p.arg(1) as u32 as u64
    } else {
        p.arg(1)
    };
    if !CRT_VALID_SIGNALS.contains(&signal) || handler == CRT_SIG_ERR {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(CRT_SIG_ERR);
    }
    let cell = crt_signal_cell(p, signal);
    let previous = p.rdptr(cell)?;
    p.wrptr(cell, handler)?;
    Ok(previous)
}

fn crt_raise(p: &mut Process) -> R {
    let signal = p.arg(0) as u32 as i32;
    if !CRT_VALID_SIGNALS.contains(&signal) {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(1);
    }
    let cell = crt_signal_cell(p, signal);
    let handler = p.rdptr(cell)?;
    match handler {
        0 => p.finish_exit(3).map(|_| 0),
        CRT_SIG_IGN => Ok(0),
        _ => {
            // Microsoft CRT handlers are one-shot, except for SIGFPE.
            if signal != 8 {
                p.wrptr(cell, 0)?;
            }
            if signal == 8 {
                p.call_guest(handler, &[signal as u64, 0])?;
            } else {
                p.call_guest(handler, &[signal as u64])?;
            }
            Ok(0)
        }
    }
}

fn crt_abort(p: &mut Process) -> R {
    let behavior = p
        .extra
        .abort_behavior_flags
        .unwrap_or(CRT_ABORT_DEFAULT_BEHAVIOR);
    if behavior & CRT_ABORT_WRITE_ABORT_MSG != 0 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "abort message for the active MSVC CRT version".into(),
        }
        .record(p));
    }

    let signal_cell = crt_signal_cell(p, 22);
    let handler = p.rdptr(signal_cell)?;
    if handler != 0 && handler != CRT_SIG_IGN {
        p.wrptr(signal_cell, 0)?;
        p.call_guest(handler, &[22])?;
    }

    if behavior & CRT_ABORT_CALL_REPORTFAULT != 0 {
        return Err(Stop::Crash(
            "untranslated: CRT abort _CALL_REPORTFAULT requires Windows Error Reporting".into(),
        ));
    }
    p.finish_exit(3).map(|_| 0)
}

trait Rec {
    fn record(self, p: &mut Process) -> Self;
}
impl Rec for Stop {
    fn record(self, p: &mut Process) -> Self {
        if let Stop::Unimplemented { dll, name } = &self {
            p.unimplemented_log.push((dll.clone(), name.clone()));
        }
        self
    }
}

impl Process {
    pub fn read_cstr_raw(&self, a: u64) -> Result<Vec<u8>, Stop> {
        self.mem
            .read_cstr(a, 1 << 24)
            .map_err(|e| Stop::Crash(format!("read fault {a:#x}: {e:?}")))
    }
    /// The `__stdio_common_vsnprintf_s` variant takes one more argument; detect it from the
    /// thunk currently executing.
    pub fn thunks_is_snprintf_s(&self) -> bool {
        let idx = ((self.cpu.rip.wrapping_sub(THUNK_BASE)) / 16) as usize;
        self.thunks
            .get(idx)
            .map(|t| t.name == "__stdio_common_vsnprintf_s")
            .unwrap_or(false)
    }
}

const CRT_STAT64I32_SIZE: u64 = 48;
const CRT_STAT_EOVERFLOW: u32 = 132;
const CRT_STAT_MAX_TIME: i64 = 32_535_215_999;

fn crt_wstat64i32(p: &mut Process) -> R {
    let (path_ptr, output) = (p.arg(0), p.arg(1));
    if path_ptr == 0 || output == 0 {
        crt_invalid_parameter(p, "_wstat64i32")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(u32::MAX as u64);
    }

    let path = p.read_wstr(path_ptr)?;
    crt_stat64i32_path(p, &path, output, "_wstat64i32")
}

fn crt_stat64i32(p: &mut Process) -> R {
    let (path_ptr, output) = (p.arg(0), p.arg(1));
    if path_ptr == 0 || output == 0 {
        crt_invalid_parameter(p, "_stat64i32")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(u32::MAX as u64);
    }
    let path = p.read_cstr(path_ptr)?;
    crt_stat64i32_path(p, &path, output, "_stat64i32")
}

fn crt_stat64i32_path(p: &mut Process, path: &str, output: u64, function: &str) -> R {
    let Some(status) = p.vfs.file_status(&path) else {
        let errno = errno_cell(p);
        p.wr32(errno, 2)?; // ENOENT
        return Ok(u32::MAX as u64);
    };

    let missing_metadata = [
        status.accessed.is_none().then_some("st_atime"),
        status.modified.is_none().then_some("st_mtime"),
        status.created.is_none().then_some("st_ctime"),
        status.mode.is_none().then_some("st_mode"),
        status.link_count.is_none().then_some("st_nlink"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if !missing_metadata.is_empty() {
        p.stderr.extend_from_slice(
            format!(
                "[coverage-gap] crt!{function} cannot translate source metadata for {:?}: {}\n",
                path,
                missing_metadata.join(", ")
            )
            .as_bytes(),
        );
        return Err(Stop::Unimplemented {
            dll: "crt".to_owned(),
            name: function.to_owned(),
        });
    }

    if status.size > i32::MAX as u64 {
        let errno = errno_cell(p);
        p.wr32(errno, CRT_STAT_EOVERFLOW)?;
        return Ok(u32::MAX as u64);
    }
    let (Some(accessed), Some(modified), Some(created), Some(source_mode), Some(link_count)) = (
        status.accessed,
        status.modified,
        status.created,
        status.mode,
        status.link_count,
    ) else {
        unreachable!("metadata completeness was checked above");
    };
    if [accessed, modified, created]
        .into_iter()
        .any(|time| !(0..=CRT_STAT_MAX_TIME).contains(&time))
        || link_count > i16::MAX as u16
    {
        let errno = errno_cell(p);
        p.wr32(errno, CRT_STAT_EOVERFLOW)?;
        return Ok(u32::MAX as u64);
    }

    let absolute_path = crate::vfs::absolute(&path, &p.current_directory);
    let drive_bytes = absolute_path.as_bytes().get(0..2);
    let drive_letter = drive_bytes
        .filter(|drive| drive[1] == b':')
        .map(|drive| drive[0].to_ascii_uppercase());
    let Some(drive) = drive_letter
        .filter(|letter| (b'A'..=b'Z').contains(letter))
        .and_then(|letter| letter.checked_sub(b'A'))
        .map(u32::from)
    else {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL for paths outside the modeled drive filesystem.
        return Ok(u32::MAX as u64);
    };

    let mut mode = if status.is_directory { 0x4000 } else { 0x8000 };
    if source_mode & 0o444 != 0 {
        mode |= 0x0100; // _S_IREAD
    }
    if source_mode & 0o222 != 0 {
        mode |= 0x0080; // _S_IWRITE
    }
    if !status.is_directory
        && matches!(
            absolute_path.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase()),
            Some(extension) if matches!(extension.as_str(), "exe" | "com" | "bat" | "cmd")
        )
    {
        mode |= 0x0040; // _S_IEXEC follows Windows executable file extensions.
    }

    // x86 MSVC _stat64i32: 32-bit device, 16-bit inode/mode/link/owner fields,
    // 32-bit file size, then three 64-bit time fields; the complete structure is 48 bytes.
    p.write_bytes(output, &[0; CRT_STAT64I32_SIZE as usize])?;
    p.wr32(output, drive)?; // st_dev
    p.wr16(output + 4, 0)?; // st_ino has no meaning on NTFS.
    p.wr16(output + 6, mode)?; // st_mode
    p.wr16(output + 8, link_count)?; // st_nlink
    p.wr16(output + 10, 0)?; // st_uid is always zero on Windows.
    p.wr16(output + 12, 0)?; // st_gid is always zero on Windows.
    p.wr32(output + 16, drive)?; // st_rdev
    p.wr32(output + 20, status.size as u32)?; // st_size
    p.wr64(output + 24, accessed as u64)?; // st_atime
    p.wr64(output + 32, modified as u64)?; // st_mtime
    p.wr64(output + 40, created as u64)?; // st_ctime
    Ok(0)
}

fn crt_wutime64(p: &mut Process) -> R {
    let (path_pointer, times_pointer) = (p.arg(0), p.arg(1));
    if path_pointer == 0 {
        crt_invalid_parameter(p, "_wutime64")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(u32::MAX as u64);
    }
    let path = p.read_wstr(path_pointer)?;
    let absolute_path = crate::vfs::absolute(&path, &p.current_directory);
    let Some(status) = p.vfs.file_status(&absolute_path) else {
        let errno = errno_cell(p);
        p.wr32(errno, 2)?; // ENOENT
        return Ok(u32::MAX as u64);
    };
    if p.vfs.is_readonly(&absolute_path) {
        let errno = errno_cell(p);
        p.wr32(errno, 13)?; // EACCES
        return Ok(u32::MAX as u64);
    }

    let (accessed, modified) = if times_pointer == 0 {
        let current = 1_767_225_600i64
            .saturating_add(((p.cpu.icount + p.extra.sleep_adjust) / 100_000_000) as i64);
        (current, current)
    } else {
        (
            p.rd64(times_pointer)? as i64,
            p.rd64(times_pointer + 8)? as i64,
        )
    };
    p.vfs.set_file_metadata(
        &absolute_path,
        crate::vfs::FileMetadata {
            accessed: Some(accessed),
            modified: Some(modified),
            created: status.created,
            mode: status.mode,
            link_count: status.link_count,
        },
    );
    p.notify_vfs_change(&absolute_path, 0x10); // FILE_NOTIFY_CHANGE_LAST_WRITE
    Ok(0)
}

fn crt_wrename(p: &mut Process) -> R {
    let (source_pointer, destination_pointer) = (p.arg(0), p.arg(1));
    if source_pointer == 0 || destination_pointer == 0 {
        crt_invalid_parameter(p, "_wrename")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(u32::MAX as u64);
    }
    let source = crate::vfs::absolute(&p.read_wstr(source_pointer)?, &p.current_directory);
    let destination =
        crate::vfs::absolute(&p.read_wstr(destination_pointer)?, &p.current_directory);
    if crate::vfs::norm(&source) == crate::vfs::norm(&destination) {
        return Ok(0);
    }
    if !p.vfs.exists(&source) {
        let errno = errno_cell(p);
        p.wr32(errno, 2)?; // ENOENT
        return Ok(u32::MAX as u64);
    }
    if p.vfs.is_dir(&source) {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_wrename of a directory".into(),
        }
        .record(p));
    }
    if p.vfs.exists(&destination) {
        let errno = errno_cell(p);
        p.wr32(errno, 17)?; // EEXIST
        return Ok(u32::MAX as u64);
    }
    if p.vfs.is_readonly(&source) {
        let errno = errno_cell(p);
        p.wr32(errno, 13)?; // EACCES
        return Ok(u32::MAX as u64);
    }
    let parent = destination
        .rsplit_once('\\')
        .map_or("", |(parent, _)| parent);
    if !p.vfs.is_dir(parent) {
        let errno = errno_cell(p);
        p.wr32(errno, 2)?; // ENOENT
        return Ok(u32::MAX as u64);
    }
    let Some(bytes) = p.vfs.get(&source).cloned() else {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_wrename of a streamed asset requires host range copying".into(),
        }
        .record(p));
    };
    let status = p.vfs.file_status(&source);
    let attributes = p.vfs.file_attributes(&source);
    p.vfs.insert(&destination, bytes);
    if let Some(attributes) = attributes {
        p.vfs.set_file_attributes(&destination, attributes);
    }
    if let Some(status) = status {
        p.vfs.set_file_metadata(
            &destination,
            crate::vfs::FileMetadata {
                accessed: status.accessed,
                modified: status.modified,
                created: status.created,
                mode: status.mode,
                link_count: status.link_count,
            },
        );
    }
    if !p.vfs.remove(&source) {
        p.vfs.remove(&destination);
        let errno = errno_cell(p);
        p.wr32(errno, 13)?; // EACCES
        return Ok(u32::MAX as u64);
    }
    p.vfs
        .set_file_metadata(&source, crate::vfs::FileMetadata::default());
    p.notify_vfs_change(&source, 0x01);
    p.notify_vfs_change(&destination, 0x01 | 0x40);
    Ok(0)
}

fn crt_wsearchenv(p: &mut Process) -> R {
    let (file_pointer, variable_pointer, result_pointer) = (p.arg(0), p.arg(1), p.arg(2));
    if file_pointer == 0 || variable_pointer == 0 || result_pointer == 0 {
        crt_invalid_parameter(p, "_wsearchenv")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }
    let file = p.read_wstr(file_pointer)?;
    let variable = p.read_wstr(variable_pointer)?;
    let directories = p
        .env
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&variable))
        .map(|(_, value)| value.as_str())
        .unwrap_or("");
    let mut candidates = Vec::new();
    candidates.push(crate::vfs::absolute(&file, &p.current_directory));
    if !file.contains(':') && !file.starts_with('\\') {
        for directory in directories
            .split(';')
            .filter(|directory| !directory.is_empty())
        {
            let joined = format!(
                "{}\\{}",
                directory.trim_end_matches(|ch| ch == '\\' || ch == '/'),
                file
            );
            candidates.push(crate::vfs::absolute(&joined, &p.current_directory));
        }
    }
    let found = candidates
        .into_iter()
        .find(|candidate| p.vfs.exists(candidate))
        .unwrap_or_default();
    p.write_wstr(result_pointer, &found)?;
    Ok(0)
}

fn crt_wunlink(p: &mut Process) -> R {
    let pointer = p.arg(0);
    if pointer == 0 {
        crt_invalid_parameter(p, "_wunlink")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(u32::MAX as u64);
    }
    let path = p.read_wstr(pointer)?;
    crt_unlink_path(p, &path)
}

fn crt_unlink_path(p: &mut Process, path: &str) -> R {
    let path = crate::vfs::absolute(path, &p.current_directory);
    let error = if !p.vfs.exists(&path) {
        Some(2) // ENOENT
    } else if p.vfs.is_dir(&path) {
        Some(21) // EISDIR
    } else if p.vfs.is_readonly(&path) {
        Some(13) // EACCES
    } else if !p.vfs.remove(&path) {
        Some(13)
    } else {
        p.notify_vfs_change(&path, 0x01); // FILE_NOTIFY_CHANGE_FILE_NAME
        return Ok(0);
    };
    let errno = errno_cell(p);
    p.wr32(errno, error.unwrap_or(13) as u32)?;
    Ok(u32::MAX as u64)
}

fn crt_mktemp(p: &mut Process) -> R {
    let template = p.arg(0);
    if template == 0 {
        crt_invalid_parameter(p, "_mktemp")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }
    let original = p.read_cstr(template)?;
    let suffix_len = original
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'X')
        .count();
    if suffix_len == 0 {
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }

    const COUNTER_KEY: &str = "crt:mktemp_counter";
    let mut counter = p.extra.data_cells.get(COUNTER_KEY).copied().unwrap_or(0);
    let prefix_len = original.len() - suffix_len;
    let suffix_capacity = 36u64.saturating_pow(suffix_len as u32);
    let attempts = suffix_capacity.min(1_000_000);
    for _ in 0..attempts {
        let mut value = counter % suffix_capacity;
        counter = counter.wrapping_add(1);
        let mut suffix = vec![b'0'; suffix_len];
        for byte in suffix.iter_mut().rev() {
            let digit = (value % 36) as u8;
            *byte = if digit < 10 {
                b'0' + digit
            } else {
                b'a' + digit - 10
            };
            value /= 36;
        }
        let mut candidate = original.as_bytes()[..prefix_len].to_vec();
        candidate.extend_from_slice(&suffix);
        let candidate = String::from_utf8(candidate)
            .map_err(|error| Stop::Crash(format!("_mktemp input was not valid UTF-8: {error}")))?;
        let absolute = crate::vfs::absolute(&candidate, &p.current_directory);
        if !p.vfs.exists(&absolute) {
            p.write_cstr(template, &candidate)?;
            p.extra.data_cells.insert(COUNTER_KEY.into(), counter);
            return Ok(template);
        }
    }
    p.extra.data_cells.insert(COUNTER_KEY.into(), counter);
    let errno = errno_cell(p);
    p.wr32(errno, 17)?; // EEXIST
    Ok(0)
}

fn errno_cell(p: &mut Process) -> u64 {
    if p.extra.errno_cell == 0 {
        p.extra.errno_cell = data_cell(p, "errno", 16);
    }
    p.extra.errno_cell
}

const MAX_LOCALTIME64: i64 = 32_535_215_999; // 3000-12-31 23:59:59 UTC

fn localtime64(p: &mut Process) -> R {
    crt_time_to_tm(p, "_localtime64")
}

fn gmtime64(p: &mut Process) -> R {
    crt_time_to_tm(p, "_gmtime64")
}

fn crt_time_to_tm(p: &mut Process, function: &str) -> R {
    let source = p.arg(0);
    if source == 0 {
        crt_invalid_parameter(p, function)?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }

    let time = p.rd64(source)? as i64;
    if !(0..=MAX_LOCALTIME64).contains(&time) {
        crt_invalid_parameter(p, function)?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }

    let thread_id = p.current_thread_id;
    let result = match p.extra.localtime_buffers.get(&thread_id).copied() {
        Some(result) => result,
        None => {
            let result = kernel32::alloc_block(p, 9 * 4, true);
            if result == 0 {
                let errno = errno_cell(p);
                p.wr32(errno, 12)?; // ENOMEM
                return Ok(0);
            }
            p.extra.localtime_buffers.insert(thread_id, result);
            result
        }
    };

    for (index, field) in broken_down_utc_time(time as u64).into_iter().enumerate() {
        p.wr32(result + index as u64 * 4, field as u32)?;
    }
    Ok(result)
}

fn crt_tm_fields(p: &Process, tm: u64) -> Result<[i32; 9], Stop> {
    let mut fields = [0; 9];
    for (index, field) in fields.iter_mut().enumerate() {
        *field = p.rd32(tm + index as u64 * 4)? as i32;
    }
    Ok(fields)
}

fn crt_tm_asctime(fields: &[i32; 9]) -> Result<String, Stop> {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let weekday = WEEKDAYS
        .get(fields[6] as usize)
        .ok_or_else(|| Stop::Unimplemented {
            dll: "crt".into(),
            name: format!(
                "asctime tm_wday {} is outside the C locale calendar",
                fields[6]
            ),
        })?;
    let month = MONTHS
        .get(fields[4] as usize)
        .ok_or_else(|| Stop::Unimplemented {
            dll: "crt".into(),
            name: format!(
                "asctime tm_mon {} is outside the C locale calendar",
                fields[4]
            ),
        })?;
    if !(0..=23).contains(&fields[2])
        || !(0..=59).contains(&fields[1])
        || !(0..=60).contains(&fields[0])
        || !(1..=31).contains(&fields[3])
    {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "asctime invalid tm calendar fields".into(),
        });
    }
    let year = fields[5] as i64 + 1900;
    if !(0..=9999).contains(&year) {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("asctime year {year} does not fit the 26-byte CRT buffer"),
        });
    }
    Ok(format!(
        "{weekday} {month} {:2} {:02}:{:02}:{:02} {year:4}\n",
        fields[3], fields[2], fields[1], fields[0]
    ))
}

fn crt_thread_static_buffer(p: &mut Process, key: &str, size: u64) -> u64 {
    let key = format!("crt:thread-static:{key}:{}", p.current_thread_id);
    if let Some(address) = p.extra.data_cells.get(&key).copied() {
        address
    } else {
        let address = kernel32::alloc_block(p, size, true);
        p.extra.data_cells.insert(key, address);
        address
    }
}

fn asctime(p: &mut Process) -> R {
    let tm = p.arg(0);
    if tm == 0 {
        crt_invalid_parameter(p, "asctime")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(0);
    }
    let formatted = crt_tm_asctime(&crt_tm_fields(p, tm)?)?;
    let buffer = crt_thread_static_buffer(p, "asctime", 26);
    p.write_cstr(buffer, &formatted)?;
    Ok(buffer)
}

fn ctime64(p: &mut Process) -> R {
    let source = p.arg(0);
    if source == 0 {
        crt_invalid_parameter(p, "_ctime64")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(0);
    }
    let time = p.rd64(source)? as i64;
    if !(0..=MAX_LOCALTIME64).contains(&time) {
        crt_invalid_parameter(p, "_ctime64")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(0);
    }
    let formatted = crt_tm_asctime(&broken_down_utc_time(time as u64))?;
    let buffer = crt_thread_static_buffer(p, "asctime", 26);
    p.write_cstr(buffer, &formatted)?;
    Ok(buffer)
}

fn difftime64(p: &mut Process) -> R {
    let (first, second) = if p.cpu.mode == CpuMode::X86_32 {
        let first = ((p.arg(1) as u32 as u64) << 32) | p.arg(0) as u32 as u64;
        let second = ((p.arg(3) as u32 as u64) << 32) | p.arg(2) as u32 as u64;
        (first as i64, second as i64)
    } else {
        (p.arg(0) as i64, p.arg(1) as i64)
    };
    crt_set_double_return(p, first as f64 - second as f64);
    Ok(0)
}

fn mktime64(p: &mut Process) -> R {
    let tm = p.arg(0);
    if tm == 0 {
        crt_invalid_parameter(p, "_mktime64")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(u64::MAX);
    }
    let fields = crt_tm_fields(p, tm)?;
    let mut year = fields[5] as i64 + 1900;
    let month_index = fields[4] as i64;
    year += month_index.div_euclid(12);
    let month = month_index.rem_euclid(12) + 1;
    let days = crt_days_from_civil(year, month, 1) + fields[3] as i128 - 1;
    let seconds =
        days * 86_400 + fields[2] as i128 * 3_600 + fields[1] as i128 * 60 + fields[0] as i128;
    if seconds < 0 || seconds > MAX_LOCALTIME64 as i128 {
        let errno = errno_cell(p);
        p.wr32(errno, 34)?;
        return Ok(u64::MAX);
    }
    let time = seconds as u64;
    for (index, field) in broken_down_utc_time(time).into_iter().enumerate() {
        p.wr32(tm + index as u64 * 4, field as u32)?;
    }
    Ok(time)
}

fn crt_days_from_civil(mut year: i64, month: i64, day: i32) -> i128 {
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era as i128 * 146_097 + day_of_era as i128) - 719_468
}

fn wctime64_s(p: &mut Process) -> R {
    let (buffer, size, source) = (p.arg(0), p.arg(1), p.arg(2));
    if buffer == 0 || source == 0 || size == 0 {
        return crt_secure_wide_string_error(p, "_wctime64_s", buffer, size, 22);
    }
    let time = p.rd64(source)? as i64;
    if !(0..=MAX_LOCALTIME64).contains(&time) {
        return crt_secure_wide_string_error(p, "_wctime64_s", buffer, size, 22);
    }
    let formatted = crt_tm_asctime(&broken_down_utc_time(time as u64))?;
    let units = formatted.encode_utf16().count() as u64;
    if size <= units {
        return crt_secure_wide_string_error(p, "_wctime64_s", buffer, size, 34);
    }
    p.write_wstr(buffer, &formatted)?;
    Ok(0)
}

fn strftime(p: &mut Process) -> R {
    let (destination, capacity, format, tm) = (p.arg(0), p.arg(1) as usize, p.arg(2), p.arg(3));
    if format == 0 || tm == 0 {
        crt_invalid_parameter(p, "strftime")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(0);
    }
    if capacity > (1 << 20) {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("strftime capacity {capacity} exceeds the translated output limit"),
        }
        .record(p));
    }
    let fields = crt_tm_fields(p, tm)?;
    let input = p.read_cstr_raw(format)?;
    let mut output = Vec::new();
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'%' {
            output.push(input[index]);
            index += 1;
            continue;
        }
        index += 1;
        let Some(&specifier) = input.get(index) else {
            output.push(b'%');
            break;
        };
        index += 1;
        let year = fields[5] as i64 + 1900;
        let rendered = match specifier {
            b'%' => "%".to_owned(),
            b'a' => crt_tm_asctime(&fields)?.get(..3).unwrap().to_owned(),
            b'A' => [
                "Sunday",
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
            ]
            .get(fields[6] as usize)
            .ok_or_else(|| Stop::Unimplemented {
                dll: "crt".into(),
                name: "strftime invalid tm_wday".into(),
            })?
            .to_string(),
            b'b' | b'h' => crt_tm_asctime(&fields)?.get(4..7).unwrap().to_owned(),
            b'B' => [
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ]
            .get(fields[4] as usize)
            .ok_or_else(|| Stop::Unimplemented {
                dll: "crt".into(),
                name: "strftime invalid tm_mon".into(),
            })?
            .to_string(),
            b'c' => crt_tm_asctime(&fields)?.trim_end().to_owned(),
            b'C' => format!("{:02}", year.div_euclid(100)),
            b'd' => format!("{:02}", fields[3]),
            b'e' => format!("{:2}", fields[3]),
            b'F' => format!("{year:04}-{:02}-{:02}", fields[4] + 1, fields[3]),
            b'H' => format!("{:02}", fields[2]),
            b'I' => format!(
                "{:02}",
                if fields[2] % 12 == 0 {
                    12
                } else {
                    fields[2] % 12
                }
            ),
            b'j' => format!("{:03}", fields[7] + 1),
            b'm' => format!("{:02}", fields[4] + 1),
            b'M' => format!("{:02}", fields[1]),
            b'n' => "\n".to_owned(),
            b'p' => if fields[2] < 12 { "AM" } else { "PM" }.to_owned(),
            b'r' => format!(
                "{}:{:02}:{:02} {}",
                if fields[2] % 12 == 0 {
                    12
                } else {
                    fields[2] % 12
                },
                fields[1],
                fields[0],
                if fields[2] < 12 { "AM" } else { "PM" }
            ),
            b'R' => format!("{:02}:{:02}", fields[2], fields[1]),
            b'S' => format!("{:02}", fields[0]),
            b't' => "\t".to_owned(),
            b'T' | b'X' => format!("{:02}:{:02}:{:02}", fields[2], fields[1], fields[0]),
            b'u' => {
                if fields[6] == 0 {
                    "7".to_owned()
                } else {
                    fields[6].to_string()
                }
            }
            b'U' => format!("{:02}", (fields[7] + 7 - fields[6]) / 7),
            b'w' => fields[6].to_string(),
            b'W' => format!("{:02}", (fields[7] + 7 - (fields[6] + 6) % 7) / 7),
            b'x' => format!(
                "{:02}/{:02}/{:02}",
                fields[4] + 1,
                fields[3],
                year.rem_euclid(100)
            ),
            b'y' => format!("{:02}", year.rem_euclid(100)),
            b'Y' => format!("{year:04}"),
            b'z' => "+0000".to_owned(),
            b'Z' => "UTC".to_owned(),
            b'E' | b'O' => {
                return Err(Stop::Unimplemented {
                    dll: "crt".into(),
                    name: format!("strftime locale modifier %{}", specifier as char),
                }
                .record(p));
            }
            other => format!("%{}", other as char),
        };
        output.extend_from_slice(rendered.as_bytes());
    }
    if destination == 0 && !output.is_empty() {
        crt_invalid_parameter(p, "strftime")?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?;
        return Ok(0);
    }
    if capacity == 0 || output.len() + 1 > capacity {
        return Ok(0);
    }
    p.write_bytes(destination, &[output.as_slice(), &[0]].concat())?;
    Ok(output.len() as u64)
}

/// Break down the UTC calendar fields used by the virtual Windows timezone (bias zero).
fn broken_down_utc_time(time: u64) -> [i32; 9] {
    const SECONDS_PER_DAY: u64 = 86_400;
    const MONTH_STARTS: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

    let days = time / SECONDS_PER_DAY;
    let seconds = time % SECONDS_PER_DAY;
    let z = days as i64 + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }

    let month_index = month as usize - 1;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let yday = MONTH_STARTS[month_index] + if leap && month > 2 { 1 } else { 0 } + day as i32 - 1;

    [
        (seconds % 60) as i32,
        (seconds / 60 % 60) as i32,
        (seconds / 3_600) as i32,
        day as i32,
        month as i32 - 1,
        year as i32 - 1_900,
        ((days + 4) % 7) as i32,
        yday,
        0,
    ]
}

fn ctype_table(p: &mut Process) -> u64 {
    if p.extra.ctype_table == 0 {
        // 257 u16 entries (index -1 .. 255); the returned pointer addresses entry 0.
        let a = kernel32::alloc_block(p, 258 * 2, true);
        for c in 0..256u32 {
            let b = c as u8;
            let mut t = 0u16;
            if b.is_ascii_uppercase() {
                t |= 0x1 | 0x100;
            }
            if b.is_ascii_lowercase() {
                t |= 0x2 | 0x100;
            }
            if b.is_ascii_digit() {
                t |= 0x4;
            }
            if matches!(b, b' ' | b'\t'..=b'\r') {
                t |= 0x8;
            }
            if b.is_ascii_punctuation() {
                t |= 0x10;
            }
            if b.is_ascii_control() {
                t |= 0x20;
            }
            if b == b' ' {
                t |= 0x40;
            }
            if b.is_ascii_hexdigit() {
                t |= 0x80;
            }
            let _ = p.mem.write_u16(a + 2 + c as u64 * 2, t);
        }
        p.extra.ctype_table = a + 2;
    }
    p.extra.ctype_table
}

/// Copy the `std::exception` message fields used by MSVC's VCRUNTIME ABI.
/// Owned messages are duplicated into guest heap memory; borrowed messages remain aliased.
fn msvc_std_exception_copy(p: &mut Process) -> R {
    let (source, destination) = (p.arg(0), p.arg(1));
    let pointer_width = guest_pointer_width(p);
    let source_what = p.rdptr(source)?;
    let source_owns_what = p.rd8(
        source
            .checked_add(pointer_width)
            .ok_or_else(|| Stop::Crash("std::exception source address overflow".into()))?,
    )? != 0;

    let (what, owns_what) = if source_owns_what && source_what != 0 {
        let mut message = p.read_cstr_raw(source_what)?;
        message.push(0);
        let allocation = kernel32::alloc_block(p, message.len() as u64, false);
        if allocation != 0 {
            if let Err(stop) = p.write_bytes(allocation, &message) {
                kernel32::free_block(p, allocation);
                return Err(stop);
            }
        }
        (allocation, true)
    } else {
        (source_what, false)
    };

    p.wrptr(destination, what)?;
    p.wr8(
        destination
            .checked_add(pointer_width)
            .ok_or_else(|| Stop::Crash("std::exception destination address overflow".into()))?,
        u8::from(owns_what),
    )?;
    Ok(0)
}

fn msvc_std_exception_destroy(p: &mut Process) -> R {
    let data = p.arg(0);
    let pointer_width = guest_pointer_width(p);
    let what = p.rdptr(data)?;
    let owns_what = p.rd8(
        data.checked_add(pointer_width)
            .ok_or_else(|| Stop::Crash("std::exception data address overflow".into()))?,
    )? != 0;
    if owns_what && what != 0 {
        kernel32::free_block(p, what);
    }
    p.wrptr(data, 0)?;
    p.wr8(
        data.checked_add(pointer_width)
            .ok_or_else(|| Stop::Crash("std::exception data address overflow".into()))?,
        0,
    )?;
    Ok(0)
}

fn msvc_purecall(p: &mut Process) -> R {
    if p.extra.purecall_handler != 0 {
        p.call_guest(p.extra.purecall_handler, &[])?;
    }
    Err(Stop::Crash(
        "pure virtual function call terminated the guest".into(),
    ))
}

fn guest_pointer_width(p: &Process) -> u64 {
    if p.cpu.mode == CpuMode::X86_32 {
        4
    } else {
        8
    }
}

fn onexit_pointer_width(p: &Process) -> u64 {
    guest_pointer_width(p)
}

fn onexit_sentinel(p: &Process) -> u64 {
    if p.cpu.mode == CpuMode::X86_32 {
        u32::MAX as u64
    } else {
        u64::MAX
    }
}

fn read_onexit_pointer(p: &Process, address: u64) -> Result<u64, Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        Ok(p.rd32(address)? as u64)
    } else {
        p.rd64(address)
    }
}

fn write_onexit_pointer(p: &mut Process, address: u64, value: u64) -> Result<(), Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        p.wr32(address, value as u32)
    } else {
        p.wr64(address, value)
    }
}

fn onexit_table_fields(p: &Process, table: u64) -> Result<[u64; 3], Stop> {
    let width = onexit_pointer_width(p);
    let second = table
        .checked_add(width)
        .ok_or_else(|| Stop::Crash("onexit table address overflow".into()))?;
    let third = second
        .checked_add(width)
        .ok_or_else(|| Stop::Crash("onexit table address overflow".into()))?;
    Ok([
        read_onexit_pointer(p, table)?,
        read_onexit_pointer(p, second)?,
        read_onexit_pointer(p, third)?,
    ])
}

fn write_onexit_table_fields(p: &mut Process, table: u64, fields: [u64; 3]) -> Result<(), Stop> {
    let width = onexit_pointer_width(p);
    for (index, value) in fields.into_iter().enumerate() {
        let address = table
            .checked_add(index as u64 * width)
            .ok_or_else(|| Stop::Crash("onexit table address overflow".into()))?;
        write_onexit_pointer(p, address, value)?;
    }
    Ok(())
}

fn initialize_onexit_table(p: &mut Process) -> Result<u64, Stop> {
    let table = p.arg(0);
    if table == 0 {
        return Ok(u64::MAX);
    }
    write_onexit_table_fields(p, table, [0; 3])?;
    Ok(0)
}

fn register_onexit_function(p: &mut Process) -> Result<u64, Stop> {
    let table = p.arg(0);
    if table == 0 {
        return Ok(u64::MAX);
    }
    let function = p.arg(1);
    let width = onexit_pointer_width(p);
    let [mut first, mut last, mut end] = onexit_table_fields(p, table)?;
    let sentinel = onexit_sentinel(p);
    if [first, last, end] == [sentinel; 3] {
        return Ok(u64::MAX);
    }
    if [first, last, end] == [0; 3] {
        let bytes = ONEXIT_INITIAL_CAPACITY * width;
        first = kernel32::alloc_block(p, bytes, false);
        if first == 0 {
            return Ok(u64::MAX);
        }
        let Some(new_end) = first.checked_add(bytes) else {
            kernel32::free_block(p, first);
            return Ok(u64::MAX);
        };
        last = first;
        end = new_end;
    } else {
        let Some(capacity_bytes) = end.checked_sub(first) else {
            return Ok(u64::MAX);
        };
        let Some(used_bytes) = last.checked_sub(first) else {
            return Ok(u64::MAX);
        };
        let Some(heap_bytes) = p.heap.size_of(first) else {
            return Ok(u64::MAX);
        };
        if first == 0
            || end < last
            || capacity_bytes == 0
            || capacity_bytes % width != 0
            || used_bytes % width != 0
            || capacity_bytes > heap_bytes
            || capacity_bytes / width > ONEXIT_MAX_ENTRIES
        {
            return Ok(u64::MAX);
        }
    }

    if last == end {
        let capacity = (end - first) / width;
        let Some(new_capacity) = capacity.checked_mul(2) else {
            return Ok(u64::MAX);
        };
        if new_capacity == 0 || new_capacity > ONEXIT_MAX_ENTRIES {
            return Ok(u64::MAX);
        }
        let used_bytes = last - first;
        let new_bytes = new_capacity * width;
        let new_first = kernel32::realloc_block(p, first, new_bytes, 0)?;
        if new_first == 0 {
            return Ok(u64::MAX);
        }
        let Some(new_end) = new_first.checked_add(new_bytes) else {
            kernel32::free_block(p, new_first);
            write_onexit_table_fields(p, table, [0; 3])?;
            return Ok(u64::MAX);
        };
        first = new_first;
        let Some(new_last) = first.checked_add(used_bytes) else {
            kernel32::free_block(p, first);
            write_onexit_table_fields(p, table, [0; 3])?;
            return Ok(u64::MAX);
        };
        last = new_last;
        end = new_end;
    }

    let Some(new_last) = last.checked_add(width) else {
        return Ok(u64::MAX);
    };
    write_onexit_pointer(p, last, function)?;
    last = new_last;
    write_onexit_table_fields(p, table, [first, last, end])?;
    Ok(0)
}

fn execute_onexit_table(p: &mut Process) -> Result<u64, Stop> {
    let table = p.arg(0);
    if table == 0 {
        return Ok(u64::MAX);
    }
    let [first, last, end] = onexit_table_fields(p, table)?;
    let sentinel = onexit_sentinel(p);
    if [first, last, end] == [sentinel; 3] {
        return Ok(u64::MAX);
    }
    let width = onexit_pointer_width(p);
    let callbacks = if [first, last, end] == [0; 3] {
        Vec::new()
    } else {
        let Some(capacity_bytes) = end.checked_sub(first) else {
            return Ok(u64::MAX);
        };
        let Some(used_bytes) = last.checked_sub(first) else {
            return Ok(u64::MAX);
        };
        let Some(heap_bytes) = p.heap.size_of(first) else {
            return Ok(u64::MAX);
        };
        if first == 0
            || end < last
            || capacity_bytes == 0
            || capacity_bytes % width != 0
            || used_bytes % width != 0
            || capacity_bytes > heap_bytes
            || used_bytes > capacity_bytes
            || capacity_bytes / width > ONEXIT_MAX_ENTRIES
        {
            return Ok(u64::MAX);
        }
        let count = used_bytes / width;
        let mut callbacks = Vec::with_capacity(count as usize);
        for index in 0..count {
            callbacks.push(read_onexit_pointer(p, first + index * width)?);
        }
        callbacks
    };

    // Mark the table invalid before callbacks run. Reuse requires initialization again.
    write_onexit_table_fields(p, table, [sentinel; 3])?;
    let mut callback_result = Ok(());
    for callback in callbacks.into_iter().rev() {
        if callback != 0 {
            if let Err(stop) = p.call_guest(callback, &[]) {
                callback_result = Err(stop);
                break;
            }
        }
    }
    if first != 0 {
        kernel32::free_block(p, first);
    }
    callback_result?;
    Ok(0)
}

fn initterm(p: &mut Process, check: bool) -> R {
    let (mut cur, end) = (p.arg(0), p.arg(1));
    let pointer_size = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    while cur < end {
        let f = p.rdptr(cur)?;
        if f != 0 {
            let r = p.call_guest(f, &[])?;
            if check && (r as u32) != 0 {
                return Ok(r & 0xffff_ffff);
            }
        }
        cur += pointer_size;
    }
    Ok(0)
}

fn strcmp_family(p: &mut Process) -> R {
    let name = {
        let idx = ((p.cpu.rip - THUNK_BASE) / 16) as usize;
        p.thunks[idx].name.clone()
    };
    let (a, b) = (p.arg(0), p.arg(1));
    let n = if name.contains('n')
        && name != "stricmp"
        && name != "_stricmp"
        && name != "strcmpi"
        && name != "_strcmpi"
        && name != "strcasecmp"
    {
        Some(p.arg(2) as usize)
    } else {
        None
    };
    let ci = name.contains("icmp") || name.contains("case") || name.contains("cmpi");
    let mut x = p.read_cstr_raw(a)?;
    let mut y = p.read_cstr_raw(b)?;
    if let Some(n) = n {
        x.truncate(n);
        y.truncate(n);
    }
    if ci {
        x.make_ascii_lowercase();
        y.make_ascii_lowercase();
    }
    Ok(x.cmp(&y) as i32 as i64 as u64)
}

fn parse_int(s: &str, base: u32) -> (i64, usize) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        neg = b[i] == b'-';
        i += 1;
    }
    let mut base = base;
    if (base == 16 || base == 0) && i + 1 < b.len() && b[i] == b'0' && (b[i + 1] | 0x20) == b'x' {
        base = 16;
        i += 2;
    } else if base == 0 {
        base = if i < b.len() && b[i] == b'0' { 8 } else { 10 };
    }
    let mut v: i64 = 0;
    let mut any = false;
    while i < b.len() {
        let d = match (b[i] as char).to_digit(36) {
            Some(d) if d < base => d,
            _ => break,
        };
        v = v.wrapping_mul(base as i64).wrapping_add(d as i64);
        any = true;
        i += 1;
    }
    if !any {
        return (0, 0);
    }
    (if neg { v.wrapping_neg() } else { v }, i)
}

fn parse_float(s: &str) -> (f64, usize) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let lower = s[i..].to_ascii_lowercase();
    if lower.starts_with("inf") {
        let v = if b[start] == b'-' {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
        return (v, i + 3);
    }
    if lower.starts_with("nan") {
        return (f64::NAN, i + 3);
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < b.len() && (b[i] | 0x20) == b'e' {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'-' || b[j] == b'+') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    match s[start..i].parse::<f64>() {
        Ok(v) => (v, i),
        Err(_) => (0.0, 0),
    }
}

fn strtol_family(p: &mut Process) -> R {
    let name = {
        let idx = ((p.cpu.rip - THUNK_BASE) / 16) as usize;
        p.thunks[idx].name.clone()
    };
    let (s, end, base) = (p.arg(0), p.arg(1), p.arg(2) as u32);
    let st = p.read_cstr(s)?;
    let (v, used) = parse_int(&st, base);
    if end != 0 {
        p.wr64(end, s + used as u64)?;
    }
    let long64 = name.contains("ll") || name.contains("64");
    Ok(if long64 {
        v as u64
    } else {
        v as i32 as u32 as u64
    })
}

fn itoa_family(p: &mut Process) -> R {
    let name = {
        let idx = ((p.cpu.rip - THUNK_BASE) / 16) as usize;
        p.thunks[idx].name.clone()
    };
    // _itoa(value, buf, radix)  /  _itoa_s(value, buf, size, radix)
    let s_variant = name.ends_with("_s");
    let (value, dst) = (p.arg(0), p.arg(1));
    let radix = if s_variant {
        p.arg(3) as u32
    } else {
        p.arg(2) as u32
    };
    let unsigned = name.contains("ul") || name.contains("ui64");
    let wide64 = name.contains("64");
    let v: i128 = match (unsigned, wide64) {
        (true, true) => value as i128,
        (true, false) => (value as u32) as i128,
        (false, true) => (value as i64) as i128,
        (false, false) => (value as i32) as i128,
    };
    let radix = if (2..=36).contains(&radix) { radix } else { 10 };
    let mut n = v.unsigned_abs();
    let mut d = Vec::new();
    if n == 0 {
        d.push(b'0');
    }
    while n > 0 {
        d.push(std::char::from_digit((n % radix as u128) as u32, radix).unwrap() as u8);
        n /= radix as u128;
    }
    if v < 0 && radix == 10 {
        d.push(b'-');
    }
    d.reverse();
    d.push(0);
    if s_variant && (p.arg(2) as usize) < d.len() {
        return Ok(34);
    }
    p.write_bytes(dst, &d)?;
    Ok(if s_variant { 0 } else { dst })
}

fn qsort(p: &mut Process) -> R {
    let (base, n, sz, cmp) = (p.arg(0), p.arg(1) as usize, p.arg(2) as usize, p.arg(3));
    let mut items: Vec<Vec<u8>> = (0..n)
        .map(|i| p.read_bytes(base + (i * sz) as u64, sz))
        .collect::<Result<_, _>>()?;
    // Insertion into a scratch area so the comparator can be a guest function taking pointers.
    let scratch = kernel32::alloc_block(p, (sz * 2) as u64, true);
    let mut err: Option<Stop> = None;
    // Merge sort with guest comparator calls.
    fn msort(
        p: &mut Process,
        v: Vec<Vec<u8>>,
        scratch: u64,
        sz: usize,
        cmp: u64,
        err: &mut Option<Stop>,
    ) -> Vec<Vec<u8>> {
        if v.len() <= 1 || err.is_some() {
            return v;
        }
        let mid = v.len() / 2;
        let (l, r) = (v[..mid].to_vec(), v[mid..].to_vec());
        let l = msort(p, l, scratch, sz, cmp, err);
        let r = msort(p, r, scratch, sz, cmp, err);
        let mut out = Vec::with_capacity(l.len() + r.len());
        let (mut i, mut j) = (0, 0);
        while i < l.len() && j < r.len() {
            if err.is_some() {
                break;
            }
            let _ = p.write_bytes(scratch, &l[i]);
            let _ = p.write_bytes(scratch + sz as u64, &r[j]);
            match p.call_guest(cmp, &[scratch, scratch + sz as u64]) {
                Ok(c) => {
                    if (c as i32) <= 0 {
                        out.push(l[i].clone());
                        i += 1;
                    } else {
                        out.push(r[j].clone());
                        j += 1;
                    }
                }
                Err(e) => *err = Some(e),
            }
        }
        out.extend_from_slice(&l[i..]);
        out.extend_from_slice(&r[j..]);
        out
    }
    items = msort(p, items, scratch, sz, cmp, &mut err);
    kernel32::free_block(p, scratch);
    if let Some(e) = err {
        return Err(e);
    }
    for (i, it) in items.iter().enumerate() {
        p.write_bytes(base + (i * sz) as u64, it)?;
    }
    Ok(0)
}

fn bsearch(p: &mut Process) -> R {
    let (key, base, n, sz, cmp) = (p.arg(0), p.arg(1), p.arg(2) as usize, p.arg(3), p.arg(4));
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let elem = base + mid as u64 * sz;
        let c = p.call_guest(cmp, &[key, elem])? as i32;
        if c == 0 {
            return Ok(elem);
        } else if c < 0 {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(0)
}

fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let e = x.abs().log2().floor() as i32 + 1;
    let mut m = x / 2f64.powi(e);
    let mut e = e;
    if m.abs() >= 1.0 {
        m /= 2.0;
        e += 1;
    }
    if m.abs() < 0.5 {
        m *= 2.0;
        e -= 1;
    }
    (m, e)
}

fn crt_ci_unary(p: &mut Process, operation: &str) -> R {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("x86 _CI{operation} calling convention outside PE32"),
        }
        .record(p));
    }
    let top = p.cpu.fpu_top as usize & 7;
    let input = p.cpu.fpu_st[top];
    let result = match operation {
        "sin" => input.sin(),
        "sqrt" => input.sqrt(),
        "cos" => input.cos(),
        "tan" => input.tan(),
        "sinh" => input.sinh(),
        "cosh" => input.cosh(),
        "tanh" => input.tanh(),
        "atan" => input.atan(),
        "exp" => input.exp(),
        "log" => input.ln(),
        "log10" => input.log10(),
        "acos" => input.acos(),
        "asin" => input.asin(),
        _ => {
            return Err(Stop::Crash(format!(
                "invalid x87 CRT unary math operation {operation}"
            )))
        }
    };
    p.cpu.fpu_st[top] = result;
    Ok(0)
}

fn crt_ecvt_fcvt(p: &mut Process, fixed: bool) -> R {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "MSVCR80 _ecvt/_fcvt use the PE32 stack-double ABI".into(),
        }
        .record(p));
    }
    let stack = p.cpu.gpr[RSP];
    let value = f64::from_bits(p.rd64(stack + 4)?);
    let digits = p.rd32(stack + 12)? as i32;
    let decimal_pointer = p.rd32(stack + 16)? as u64;
    let sign_pointer = p.rd32(stack + 20)? as u64;
    let function = if fixed { "_fcvt" } else { "_ecvt" };
    if digits < 0 || decimal_pointer == 0 || sign_pointer == 0 {
        crt_invalid_parameter(p, function)?;
        let errno = errno_cell(p);
        p.wr32(errno, 22)?; // EINVAL
        return Ok(0);
    }
    if digits > 256 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("{function} requested precision {digits} exceeds the modeled buffer"),
        }
        .record(p));
    }
    if !value.is_finite() {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("{function} non-finite CRT spelling"),
        }
        .record(p));
    }

    let sign = value.is_sign_negative() as u32;
    let absolute = value.abs();
    let (text, decimal_point) = if fixed {
        let rendered = format!("{:.*}", digits as usize, absolute);
        let decimal_point = rendered.find('.').unwrap_or(rendered.len());
        (rendered.replace('.', ""), decimal_point as i32)
    } else if digits == 0 {
        (String::new(), 0)
    } else {
        let rendered = format!("{:.*e}", digits.saturating_sub(1) as usize, absolute);
        let (mantissa, exponent) = rendered
            .split_once('e')
            .ok_or_else(|| Stop::Crash(format!("{function} formatter omitted exponent")))?;
        let exponent = exponent.parse::<i32>().map_err(|error| {
            Stop::Crash(format!(
                "{function} formatter returned bad exponent: {error}"
            ))
        })?;
        (mantissa.replace('.', ""), exponent + 1)
    };
    if text.len() >= 512 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!(
                "{function} output length {} exceeds the modeled static buffer",
                text.len()
            ),
        }
        .record(p));
    }
    let buffer = crt_thread_static_buffer(p, "_cvt", 512);
    p.write_cstr(buffer, &text)?;
    p.wr32(decimal_pointer, decimal_point as u32)?;
    p.wr32(sign_pointer, sign)?;
    Ok(buffer)
}

fn crt_untranslated_import(p: &mut Process, name: &str, limitation: &str) -> R {
    Err(Stop::Unimplemented {
        dll: "crt".into(),
        name: format!("{name}: {limitation}"),
    }
    .record(p))
}

fn crt_setvbuf(p: &mut Process) -> R {
    let (stream, mode) = (p.arg(0), p.arg(2) as u32);
    if !p.extra.files.contains_key(&stream) {
        return Ok(u32::MAX as u64);
    }
    match mode {
        2 => {
            // CRT streams currently commit writes immediately, which is exactly _IONBF behavior.
            Ok(0)
        }
        0 | 1 => crt_untranslated_import(
            p,
            "setvbuf",
            "full/line buffering and caller buffer lifetime",
        ),
        _ => {
            let errno = errno_cell(p);
            p.wr32(errno, 22)?; // EINVAL
            Ok(u32::MAX as u64)
        }
    }
}

fn crt_setusermatherr(p: &mut Process) -> R {
    if p.arg(0) != 0 {
        return crt_untranslated_import(
            p,
            "__setusermatherr",
            "callback invocation on math exceptions",
        );
    }
    p.extra.data_cells.remove("crt:usermatherr");
    Ok(0)
}

fn math1(p: &mut Process) -> R {
    let name = {
        let idx = ((p.cpu.rip - THUNK_BASE) / 16) as usize;
        p.thunks[idx].name.clone()
    };
    let x = p.argf(0);
    let r = match name.as_str() {
        "sqrt" => x.sqrt(),
        "sin" => x.sin(),
        "cos" => x.cos(),
        "tan" => x.tan(),
        "atan" => x.atan(),
        "asin" => x.asin(),
        "acos" => x.acos(),
        "sinh" => x.sinh(),
        "cosh" => x.cosh(),
        "tanh" => x.tanh(),
        "exp" => x.exp(),
        "log" => x.ln(),
        "log10" => x.log10(),
        "log2" => x.log2(),
        "floor" => x.floor(),
        "ceil" => x.ceil(),
        "fabs" => x.abs(),
        "round" => x.round(),
        "trunc" => x.trunc(),
        "cbrt" => x.cbrt(),
        "exp2" => x.exp2(),
        "expm1" => x.exp_m1(),
        "log1p" => x.ln_1p(),
        _ => f64::NAN,
    };
    p.set_retf(r);
    Ok(0)
}

fn math2(p: &mut Process) -> R {
    let name = {
        let idx = ((p.cpu.rip - THUNK_BASE) / 16) as usize;
        p.thunks[idx].name.clone()
    };
    let (x, y) = (p.argf(0), p.argf(1));
    let r = match name.as_str() {
        "pow" => x.powf(y),
        "atan2" => x.atan2(y),
        "fmod" => x % y,
        "hypot" => x.hypot(y),
        "fmin" => x.min(y),
        "fmax" => x.max(y),
        "copysign" => x.copysign(y),
        _ => f64::NAN,
    };
    p.set_retf(r);
    Ok(0)
}

fn math1f(p: &mut Process) -> R {
    let name = {
        let idx = ((p.cpu.rip - THUNK_BASE) / 16) as usize;
        p.thunks[idx].name.clone()
    };
    let x = f32::from_bits(p.cpu.xmm[0] as u32);
    let r = match name.trim_end_matches('f') {
        "sqrt" => x.sqrt(),
        "sin" => x.sin(),
        "cos" => x.cos(),
        "tan" => x.tan(),
        "atan" => x.atan(),
        "asin" => x.asin(),
        "acos" => x.acos(),
        "exp" => x.exp(),
        "log" => x.ln(),
        "log10" => x.log10(),
        "log2" => x.log2(),
        "floor" => x.floor(),
        "ceil" => x.ceil(),
        "fabs" => x.abs(),
        "round" => x.round(),
        "trunc" => x.trunc(),
        "sinh" => x.sinh(),
        "cosh" => x.cosh(),
        "tanh" => x.tanh(),
        _ => f32::NAN,
    };
    p.set_retf32(r);
    Ok(0)
}

fn math2f(p: &mut Process) -> R {
    let name = {
        let idx = ((p.cpu.rip - THUNK_BASE) / 16) as usize;
        p.thunks[idx].name.clone()
    };
    let x = f32::from_bits(p.cpu.xmm[0] as u32);
    let y = f32::from_bits(p.cpu.xmm[1] as u32);
    let r = match name.trim_end_matches('f') {
        "pow" => x.powf(y),
        "atan2" => x.atan2(y),
        "fmod" => x % y,
        "hypot" => x.hypot(y),
        "fmin" => x.min(y),
        "fmax" => x.max(y),
        _ => f32::NAN,
    };
    p.set_retf32(r);
    Ok(0)
}

fn fopen_impl(p: &mut Process, name: &str, mode: &str) -> R {
    let path = crate::vfs::absolute(name, &p.current_directory);
    let write = mode.contains('w') || mode.contains('a') || mode.contains('+');
    let exists = p.vfs.exists(&path);
    if exists && write && p.vfs.is_readonly(&path) {
        return Ok(0);
    }
    let old_len = p.vfs.file_len(&path).unwrap_or(0);
    let created = !exists && (mode.starts_with('w') || mode.starts_with('a'));
    let truncated = exists && mode.starts_with('w');
    if mode.starts_with('w') {
        p.vfs.insert(&path, Vec::new());
    } else if !exists {
        if mode.starts_with('a') {
            p.vfs.insert(&path, Vec::new());
        } else {
            return Ok(0);
        }
    }
    if created {
        p.notify_vfs_change(&path, 0x01 | 0x40); // FILE_NAME | CREATION
    } else if truncated {
        let filter = 0x10 | if old_len != 0 { 0x08 } else { 0 };
        p.notify_vfs_change(&path, filter); // LAST_WRITE and SIZE when length changed
    }
    let pos = if mode.starts_with('a') {
        p.vfs.get(&path).map(|d| d.len()).unwrap_or(0)
    } else {
        0
    };
    let fp = kernel32::alloc_block(p, FILE_SIZE, true);
    p.extra.files.insert(
        fp,
        CFile {
            kind: FileKind::Vfs {
                path: crate::vfs::norm(&path),
                pos,
                read: mode.starts_with('r') || mode.contains('+'),
                write,
            },
            eof: false,
            err: false,
            ungot: None,
        },
    );
    Ok(fp)
}

const CRT_FD_KEY_PREFIX: &str = "crt:fd:";
const CRT_FD_MODE_KEY_PREFIX: &str = "crt:fd_mode:";
const CRT_O_APPEND: u32 = 0x0008;
const CRT_O_CREAT: u32 = 0x0100;
const CRT_O_TRUNC: u32 = 0x0200;
const CRT_O_EXCL: u32 = 0x0400;
const CRT_O_TEXT: u32 = 0x4000;
const CRT_O_BINARY: u32 = 0x8000;

fn crt_fd_key(prefix: &str, fd: i32) -> String {
    format!("{prefix}{fd}")
}

fn crt_fileno(p: &mut Process) -> R {
    let stream = p.arg(0);
    if let Some(kind) = p.extra.files.get(&stream).map(|file| &file.kind) {
        let standard = match kind {
            FileKind::Stdin => Some(0),
            FileKind::Stdout => Some(1),
            FileKind::Stderr => Some(2),
            FileKind::Vfs { .. } => None,
        };
        if let Some(fd) = standard {
            return Ok(fd);
        }
    }
    for (key, file) in &p.extra.data_cells {
        if *file == stream {
            if let Some(fd) = key
                .strip_prefix(CRT_FD_KEY_PREFIX)
                .and_then(|fd| fd.parse::<i32>().ok())
            {
                return Ok(fd as u32 as u64);
            }
        }
    }
    Ok(u32::MAX as u64)
}

fn crt_fd_file(p: &mut Process, fd: i32) -> Result<Option<u64>, Stop> {
    if fd < 0 {
        return Ok(None);
    }
    let key = crt_fd_key(CRT_FD_KEY_PREFIX, fd);
    if let Some(file) = p.extra.data_cells.get(&key).copied() {
        return Ok((file != 0 && p.extra.files.contains_key(&file)).then_some(file));
    }
    if (0..=2).contains(&fd) {
        let iob = ensure_iob(p)?;
        let file = iob + fd as u64 * FILE_SIZE;
        return Ok(p.extra.files.contains_key(&file).then_some(file));
    }
    Ok(None)
}

fn crt_open_error(p: &mut Process, errno_code: u32) -> Result<u64, Stop> {
    let errno = errno_cell(p);
    p.wr32(errno, errno_code)?;
    Ok(u32::MAX as u64)
}

fn crt_open(p: &mut Process) -> R {
    let (name_pointer, flags) = (p.arg(0), p.arg(1) as u32);
    if name_pointer == 0 {
        crt_invalid_parameter(p, "_open")?;
        return crt_open_error(p, 22);
    }
    let name = p.read_cstr(name_pointer)?;
    let access = flags & 3;
    if access == 3 {
        return crt_open_error(p, 22);
    }
    let known =
        3 | CRT_O_APPEND | CRT_O_CREAT | CRT_O_TRUNC | CRT_O_EXCL | CRT_O_TEXT | CRT_O_BINARY;
    if flags & !known != 0 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: format!("_open flags {flags:#x} include unmodeled CRT flags"),
        }
        .record(p));
    }
    if flags & CRT_O_BINARY == 0 || flags & CRT_O_TEXT != 0 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_open text mode requires CRLF and Ctrl-Z translation".into(),
        }
        .record(p));
    }
    let create = flags & CRT_O_CREAT != 0;
    let truncate = flags & CRT_O_TRUNC != 0;
    let exclusive = flags & CRT_O_EXCL != 0;
    let append = flags & CRT_O_APPEND != 0;
    if exclusive && !create {
        return crt_open_error(p, 22);
    }
    if truncate && access == 0 {
        return crt_open_error(p, 13); // EACCES: truncation needs write access.
    }

    let path = crate::vfs::absolute(&name, &p.current_directory);
    let exists = p.vfs.exists(&path);
    if exists && exclusive {
        return crt_open_error(p, 17); // EEXIST
    }
    if exists && p.vfs.is_dir(&path) {
        return crt_open_error(p, 21); // EISDIR
    }
    if exists && access != 0 && p.vfs.is_readonly(&path) {
        return crt_open_error(p, 13); // EACCES
    }
    if !exists && !create {
        return crt_open_error(p, 2); // ENOENT
    }
    if !exists {
        let parent = path.rsplit_once('\\').map_or("", |(parent, _)| parent);
        if !p.vfs.is_dir(parent) {
            return crt_open_error(p, 2); // ENOENT: parent directory is absent.
        }
    }

    let old_len = p.vfs.file_len(&path).unwrap_or(0);
    let created = !exists;
    if created || truncate {
        p.vfs.insert(&path, Vec::new());
        if created {
            p.notify_vfs_change(&path, 0x01 | 0x40); // FILE_NAME | CREATION
        } else {
            p.notify_vfs_change(&path, 0x10 | if old_len != 0 { 0x08 } else { 0 });
        }
    }
    let pos = if append {
        p.vfs.file_len(&path).unwrap_or(0) as usize
    } else {
        0
    };
    let file = kernel32::alloc_block(p, FILE_SIZE, true);
    p.extra.files.insert(
        file,
        CFile {
            kind: FileKind::Vfs {
                path: crate::vfs::norm(&path),
                pos,
                read: access != 1,
                write: access != 0,
            },
            eof: false,
            err: false,
            ungot: None,
        },
    );

    let mut fd = 3i32;
    while p
        .extra
        .data_cells
        .get(&crt_fd_key(CRT_FD_KEY_PREFIX, fd))
        .is_some_and(|existing| *existing != 0)
    {
        fd = fd
            .checked_add(1)
            .ok_or_else(|| Stop::Crash("CRT descriptor space exhausted".into()))?;
    }
    p.extra
        .data_cells
        .insert(crt_fd_key(CRT_FD_KEY_PREFIX, fd), file);
    p.extra
        .data_cells
        .insert(crt_fd_key(CRT_FD_MODE_KEY_PREFIX, fd), CRT_O_BINARY as u64);
    Ok(fd as u32 as u64)
}

fn crt_read(p: &mut Process) -> R {
    let fd = p.arg(0) as u32 as i32;
    let (buffer, count) = (p.arg(1), p.arg(2));
    let Some(file) = crt_fd_file(p, fd)? else {
        return crt_open_error(p, 9); // EBADF
    };
    if count != 0 && buffer == 0 {
        return crt_open_error(p, 22); // EINVAL
    }
    if matches!(
        p.extra.files.get(&file).map(|file| &file.kind),
        Some(FileKind::Vfs { read: false, .. } | FileKind::Stdout | FileKind::Stderr)
    ) {
        return crt_open_error(p, 9); // EBADF
    }
    let Ok(count) = usize::try_from(count) else {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_read count exceeds host address space".into(),
        }
        .record(p));
    };
    let data = stream_read(p, file, count)?;
    p.write_bytes(buffer, &data)?;
    Ok(data.len() as u64)
}

fn crt_write(p: &mut Process) -> R {
    let fd = p.arg(0) as u32 as i32;
    let (buffer, count) = (p.arg(1), p.arg(2));
    if fd == 0 {
        return crt_open_error(p, 9); // EBADF
    }
    if count != 0 && buffer == 0 {
        return crt_open_error(p, 22); // EINVAL
    }
    let Ok(count) = usize::try_from(count) else {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_write count exceeds host address space".into(),
        }
        .record(p));
    };
    let data = p.read_bytes(buffer, count)?;
    if fd == 1 {
        p.stdout.extend_from_slice(&data);
        return Ok(data.len() as u64);
    }
    if fd == 2 {
        p.stderr.extend_from_slice(&data);
        return Ok(data.len() as u64);
    }
    let Some(file) = crt_fd_file(p, fd)? else {
        return crt_open_error(p, 9); // EBADF
    };
    if matches!(
        p.extra.files.get(&file).map(|file| &file.kind),
        Some(FileKind::Vfs { write: false, .. } | FileKind::Stdin)
    ) {
        return crt_open_error(p, 9); // EBADF
    }
    let written = stream_write(p, file, &data)?;
    Ok(written as u64)
}

fn crt_close(p: &mut Process) -> R {
    let fd = p.arg(0) as u32 as i32;
    if fd < 3 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_close on a standard descriptor would need to disable every stdio alias".into(),
        }
        .record(p));
    }
    let key = crt_fd_key(CRT_FD_KEY_PREFIX, fd);
    let Some(file) = p.extra.data_cells.remove(&key) else {
        return crt_open_error(p, 9); // EBADF
    };
    p.extra
        .data_cells
        .remove(&crt_fd_key(CRT_FD_MODE_KEY_PREFIX, fd));
    p.extra.files.remove(&file);
    kernel32::free_block(p, file);
    Ok(0)
}

fn crt_lseek(p: &mut Process) -> R {
    let fd = p.arg(0) as u32 as i32;
    let (offset, whence) = (p.arg(1) as u32 as i32 as i64, p.arg(2) as u32);
    let Some(file) = crt_fd_file(p, fd)? else {
        return crt_open_error(p, 9); // EBADF
    };
    let Some(CFile {
        kind: FileKind::Vfs { path, pos, .. },
        ..
    }) = p.extra.files.get(&file)
    else {
        return crt_open_error(p, 9); // ESPIPE is not modeled for standard streams.
    };
    let current = *pos as i64;
    let end = p.vfs.file_len(path).unwrap_or(0) as i64;
    let base = match whence {
        0 => 0i64,
        1 => current,
        2 => end,
        _ => return crt_open_error(p, 22), // EINVAL
    };
    let Some(new_position) = base
        .checked_add(offset)
        .filter(|position| *position >= 0 && *position <= i32::MAX as i64)
    else {
        return crt_open_error(p, 22); // EINVAL / unrepresentable 32-bit offset
    };
    let Some(CFile {
        kind: FileKind::Vfs { pos, .. },
        eof,
        ungot,
        ..
    }) = p.extra.files.get_mut(&file)
    else {
        return crt_open_error(p, 9); // descriptor changed while seeking
    };
    *pos = new_position as usize;
    *eof = false;
    *ungot = None;
    Ok(new_position as u64)
}

fn crt_setmode(p: &mut Process) -> R {
    let (fd, mode) = (p.arg(0) as u32 as i32, p.arg(1) as u32);
    let Some(_file) = crt_fd_file(p, fd)? else {
        return crt_open_error(p, 9); // EBADF
    };
    let key = crt_fd_key(CRT_FD_MODE_KEY_PREFIX, fd);
    let old_mode = p.extra.data_cells.get(&key).copied().unwrap_or(0) as u32;
    if mode == old_mode {
        return Ok(old_mode as u64);
    }
    if mode == CRT_O_TEXT || mode == CRT_O_BINARY {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_setmode conversion would change CRLF and Ctrl-Z translation semantics".into(),
        }
        .record(p));
    }
    crt_open_error(p, 22)
}

fn crt_get_osfhandle(p: &mut Process) -> R {
    let fd = p.arg(0) as u32 as i32;
    if !(0..=2).contains(&fd) {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_get_osfhandle for a VFS descriptor without a translated Win32 file handle"
                .into(),
        }
        .record(p));
    }
    let handles = p.std_handles;
    let handle = handles[fd as usize];
    Ok(if handle == 0 { u32::MAX as u64 } else { handle })
}

fn crt_fstat64i32(p: &mut Process) -> R {
    let (fd, output) = (p.arg(0) as u32 as i32, p.arg(1));
    if output == 0 {
        crt_invalid_parameter(p, "_fstat64i32")?;
        return crt_open_error(p, 22); // EINVAL
    }
    let Some(file) = crt_fd_file(p, fd)? else {
        return crt_open_error(p, 9); // EBADF
    };
    let Some(CFile {
        kind: FileKind::Vfs { path, .. },
        ..
    }) = p.extra.files.get(&file)
    else {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_fstat64i32 for a standard device stream".into(),
        }
        .record(p));
    };
    let path = path.clone();
    crt_stat64i32_path(p, &path, output, "_fstat64i32")
}

/// Where scanf-family conversions find their destination pointers.
#[derive(Clone, Copy)]
enum ScanArgs {
    /// Variadic call: destinations follow the format as the 3rd, 4th, ... Win64 arguments.
    Variadic,
    /// `va_list` call: x64 MSVC `va_list` is a pointer to consecutive 8-byte argument slots.
    VaList(u64),
}

/// Minimal sscanf: %d %i %u %x %ld %lld %f %lf %s %c %n and literal matching.
fn sscanf(p: &mut Process) -> R {
    scan_core(p, p.arg(0), p.arg(1), ScanArgs::Variadic)
}

/// `int vsscanf(const char *buffer, const char *format, va_list args)` with the x64 MSVC `va_list`
/// representation. Shares every conversion with `sscanf`.
fn vsscanf(p: &mut Process) -> R {
    scan_core(p, p.arg(0), p.arg(1), ScanArgs::VaList(p.arg(2)))
}

fn scan_core(p: &mut Process, buffer: u64, format: u64, args: ScanArgs) -> R {
    let (s, f) = (p.read_cstr_raw(buffer)?, p.read_cstr_raw(format)?);
    let mut si = 0usize;
    let mut slot = 0usize;
    macro_rules! next_dst {
        () => {{
            let dst = match args {
                ScanArgs::Variadic => p.arg(2 + slot),
                ScanArgs::VaList(list) => p.rd64(list + 8 * slot as u64)?,
            };
            slot += 1;
            dst
        }};
    }
    let mut count = 0u64;
    let mut fi = 0usize;
    while fi < f.len() {
        let c = f[fi];
        if c.is_ascii_whitespace() {
            while si < s.len() && s[si].is_ascii_whitespace() {
                si += 1;
            }
            fi += 1;
            continue;
        }
        if c != b'%' {
            if si < s.len() && s[si] == c {
                si += 1;
                fi += 1;
                continue;
            }
            break;
        }
        fi += 1;
        let mut long = 0;
        while fi < f.len() && matches!(f[fi], b'l' | b'h' | b'z' | b'L') {
            if f[fi] == b'l' || f[fi] == b'z' || f[fi] == b'L' {
                long += 1;
            }
            fi += 1;
        }
        let Some(&conv) = f.get(fi) else { break };
        fi += 1;
        while si < s.len() && s[si].is_ascii_whitespace() && conv != b'c' {
            si += 1;
        }
        let rest = String::from_utf8_lossy(&s[si.min(s.len())..]).into_owned();
        match conv {
            b'd' | b'i' | b'u' | b'x' | b'X' => {
                let base = if conv == b'x' || conv == b'X' {
                    16
                } else if conv == b'i' {
                    0
                } else {
                    10
                };
                let (v, used) = parse_int(&rest, base);
                if used == 0 {
                    break;
                }
                let dst = next_dst!();
                if long >= 1 {
                    p.wr64(dst, v as u64)?
                } else {
                    p.wr32(dst, v as u32)?
                }
                si += used;
                count += 1;
            }
            b'f' | b'e' | b'g' => {
                let (v, used) = parse_float(&rest);
                if used == 0 {
                    break;
                }
                let dst = next_dst!();
                if long >= 1 {
                    p.wr64(dst, v.to_bits())?
                } else {
                    p.wr32(dst, (v as f32).to_bits())?
                }
                si += used;
                count += 1;
            }
            b's' => {
                let end = s[si..]
                    .iter()
                    .position(|b| b.is_ascii_whitespace())
                    .map(|i| si + i)
                    .unwrap_or(s.len());
                if end == si {
                    break;
                }
                let dst = next_dst!();
                let mut v = s[si..end].to_vec();
                v.push(0);
                p.write_bytes(dst, &v)?;
                si = end;
                count += 1;
            }
            b'c' => {
                if si >= s.len() {
                    break;
                }
                let dst = next_dst!();
                p.wr8(dst, s[si])?;
                si += 1;
                count += 1;
            }
            b'n' => {
                let dst = next_dst!();
                p.wr32(dst, si as u32)?;
            }
            b'%' => {
                if si < s.len() && s[si] == b'%' {
                    si += 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    if count == 0 && si >= s.len() && !f.is_empty() {
        return Ok(u64::MAX);
    }
    Ok(count)
}

// setjmp / longjmp ---------------------------------------------------------------------------
// The x64 buffer follows the runtime's opaque x64 register snapshot. The x86 path uses the
// documented MSVC `_JUMP_BUFFER` layout from setjmp.h.

fn setjmp(p: &mut Process) -> R {
    if p.cpu.mode == CpuMode::X86_32 {
        return setjmp3_x86(p, 0);
    }
    let buf = p.arg(0);
    let sp = p.cpu.gpr[RSP];
    let ra = p.rd64(sp)?;
    let regs = [
        p.cpu.gpr[RBX],
        p.cpu.gpr[RBP],
        p.cpu.gpr[RDI],
        p.cpu.gpr[RSI],
        p.cpu.gpr[12],
        p.cpu.gpr[13],
        p.cpu.gpr[14],
        p.cpu.gpr[15],
        sp + 8,
        ra,
    ];
    for (i, r) in regs.iter().enumerate() {
        p.wr64(buf + 8 * i as u64, *r)?;
    }
    for i in 0..10 {
        let x = p.cpu.xmm[6 + i];
        p.mem
            .write_u128(buf + 0x50 + 16 * i as u64, x)
            .map_err(|e| Stop::Crash(format!("{e:?}")))?;
    }
    Ok(0)
}

const X86_JUMP_COOKIE: u32 = 0x5643_3230; // MSVC VC20 jump buffer
const X86_TRYLEVEL_END: u32 = u32::MAX;

fn setjmp3(p: &mut Process) -> R {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_setjmp3 for non-PE32 process".into(),
        });
    }
    let count = p.arg(1) as u32 as i32;
    if count != 0 {
        p.stderr.extend_from_slice(
            format!(
                "[coverage-gap] crt!_setjmp3 optional unwind arguments are not translated (count={count})\n"
            )
            .as_bytes(),
        );
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_setjmp3".into(),
        });
    }
    setjmp3_x86(p, count as u32)
}

fn setjmp3_x86(p: &mut Process, count: u32) -> R {
    if count != 0 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "_setjmp3 optional unwind arguments".into(),
        });
    }
    let buffer = p.arg(0) as u32 as u64;
    let stack = p.cpu.gpr[RSP] as u32 as u64;
    let return_address = p.rd32(stack)?;
    let saved_stack = stack
        .checked_add(4)
        .ok_or_else(|| Stop::Crash("_setjmp3 stack pointer overflow".into()))?;
    let registration = p.rd32(crate::process::TEB_ADDR)?;
    let try_level = if registration == X86_TRYLEVEL_END {
        X86_TRYLEVEL_END
    } else {
        p.rd32(registration as u64 + 12)?
    };

    // MSVC x86 _JUMP_BUFFER: Ebp, Ebx, Edi, Esi, Esp, Eip, Registration,
    // TryLevel, Cookie, UnwindFunc, then six UnwindData DWORDs (64 bytes total).
    for (offset, value) in [
        (0, p.cpu.gpr[RBP] as u32),
        (4, p.cpu.gpr[RBX] as u32),
        (8, p.cpu.gpr[RDI] as u32),
        (12, p.cpu.gpr[RSI] as u32),
        (16, saved_stack as u32),
        (20, return_address),
        (24, registration),
        (28, try_level),
        (32, X86_JUMP_COOKIE),
        (36, 0),
        (40, 0),
        (44, 0),
        (48, 0),
        (52, 0),
        (56, 0),
        (60, 0),
    ] {
        p.wr32(buffer + offset, value)?;
    }
    Ok(0)
}

fn longjmp(p: &mut Process) -> R {
    if p.cpu.mode == CpuMode::X86_32 {
        return longjmp_x86(p);
    }
    let (buf, val) = (p.arg(0), p.arg(1) as u32);
    let mut regs = [0u64; 10];
    for (i, r) in regs.iter_mut().enumerate() {
        *r = p.rd64(buf + 8 * i as u64)?;
    }
    for i in 0..10 {
        p.cpu.xmm[6 + i] = p
            .mem
            .read_u128(buf + 0x50 + 16 * i as u64)
            .map_err(|e| Stop::Crash(format!("{e:?}")))?;
    }
    p.cpu.gpr[RBX] = regs[0];
    p.cpu.gpr[RBP] = regs[1];
    p.cpu.gpr[RDI] = regs[2];
    p.cpu.gpr[RSI] = regs[3];
    p.cpu.gpr[12] = regs[4];
    p.cpu.gpr[13] = regs[5];
    p.cpu.gpr[14] = regs[6];
    p.cpu.gpr[15] = regs[7];
    p.cpu.gpr[RSP] = regs[8];
    p.cpu.rip = regs[9];
    p.cpu.gpr[RAX] = if val == 0 { 1 } else { val as u64 };
    p.skip_ret = true;
    Ok(0)
}

fn longjmp_x86(p: &mut Process) -> R {
    let buffer = p.arg(0) as u32 as u64;
    let value = p.arg(1) as u32;
    let saved_registration = p.rd32(buffer + 24)?;
    let saved_try_level = p.rd32(buffer + 28)?;
    let cookie = p.rd32(buffer + 32)?;
    let unwind_function = p.rd32(buffer + 36)?;
    if cookie != X86_JUMP_COOKIE {
        return Err(Stop::Crash(format!(
            "longjmp rejected an invalid MSVC x86 jump-buffer cookie {cookie:#010x}"
        )));
    }

    let current_registration = p.rd32(crate::process::TEB_ADDR)?;
    if unwind_function != 0 || saved_registration != current_registration {
        p.stderr.extend_from_slice(
            format!(
                "[coverage-gap] crt!longjmp requires untranslated unwind state (saved registration={saved_registration:#010x}, current={current_registration:#010x}, unwind function={unwind_function:#010x})\n"
            )
            .as_bytes(),
        );
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "longjmp".into(),
        });
    }
    let current_try_level = if current_registration == X86_TRYLEVEL_END {
        X86_TRYLEVEL_END
    } else {
        p.rd32(current_registration as u64 + 12)?
    };
    if saved_try_level != current_try_level {
        p.stderr.extend_from_slice(
            format!(
                "[coverage-gap] crt!longjmp requires local EH unwind (saved try level={saved_try_level:#010x}, current={current_try_level:#010x})\n"
            )
            .as_bytes(),
        );
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "longjmp".into(),
        });
    }
    for index in 0..6 {
        if p.rd32(buffer + 40 + index * 4)? != 0 {
            p.stderr.extend_from_slice(
                b"[coverage-gap] crt!longjmp has nonempty optional unwind data that is not translated\n",
            );
            return Err(Stop::Unimplemented {
                dll: "crt".into(),
                name: "longjmp".into(),
            });
        }
    }

    p.cpu.gpr[RBP] = p.rd32(buffer)? as u64;
    p.cpu.gpr[RBX] = p.rd32(buffer + 4)? as u64;
    p.cpu.gpr[RDI] = p.rd32(buffer + 8)? as u64;
    p.cpu.gpr[RSI] = p.rd32(buffer + 12)? as u64;
    p.cpu.gpr[RSP] = p.rd32(buffer + 16)? as u64;
    p.cpu.rip = p.rd32(buffer + 20)? as u64;
    p.cpu.gpr[RAX] = if value == 0 { 1 } else { value as u64 };
    p.skip_ret = true;
    Ok(0)
}

#[cfg(test)]
mod initterm_tests {
    use super::initterm;
    use crate::process::Process;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW, PROT_RWX};

    #[test]
    fn pe32_initterm_walks_four_byte_function_pointer_entries() {
        const CODE_BASE: u64 = 0x40000;
        const STACK_BASE: u64 = 0x80000;
        const STACK: u64 = STACK_BASE + 0x800;
        const TABLE: u64 = CODE_BASE + 0x100;
        const FIRST_MARKER: u64 = CODE_BASE + 0x200;
        const SECOND_MARKER: u64 = CODE_BASE + 0x204;
        const FIRST_FUNC: u64 = CODE_BASE + 0x300;
        const SECOND_FUNC: u64 = CODE_BASE + 0x320;

        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(CODE_BASE, PAGE_SIZE, PROT_RWX));
        assert!(p.mem.map(STACK_BASE, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;

        for (function, marker, value) in [
            (FIRST_FUNC, FIRST_MARKER, 0x1357_2468u32),
            (SECOND_FUNC, SECOND_MARKER, 0x89ab_cdefu32),
        ] {
            let mut code = vec![0xc7, 0x05]; // mov dword ptr [absolute address], immediate
            code.extend_from_slice(&(marker as u32).to_le_bytes());
            code.extend_from_slice(&value.to_le_bytes());
            code.push(0xc3);
            p.write_bytes(function, &code).unwrap();
        }
        p.wr32(TABLE, FIRST_FUNC as u32).unwrap();
        p.wr32(TABLE + 4, SECOND_FUNC as u32).unwrap();
        p.wr32(STACK, 0).unwrap();
        p.wr32(STACK + 4, TABLE as u32).unwrap();
        p.wr32(STACK + 8, (TABLE + 8) as u32).unwrap();

        assert_eq!(initterm(&mut p, false), Ok(0));
        assert_eq!(p.rd32(FIRST_MARKER).unwrap(), 0x1357_2468);
        assert_eq!(p.rd32(SECOND_MARKER).unwrap(), 0x89ab_cdef);
    }
}

#[cfg(test)]
mod msvcp80_x86_import_tests {
    use super::{data_import, lookup, MSVC_STD_CIN_SYMBOL, MSVC_STD_COUT_SYMBOL};
    use crate::process::{Process, Stop};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RCX, RSP};
    use anywasm_cpu::mem::{PROT_RW, PROT_RWX};

    const STREAMBUF_SPUTC: &str = "?sputc@?$basic_streambuf@DU?$char_traits@D@std@@@std@@QAEHD@Z";
    const STREAMBUF_SPUTN: &str =
        "?sputn@?$basic_streambuf@DU?$char_traits@D@std@@@std@@QAEHPBDH@Z";
    const OSTREAM_INT: &str = "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV01@H@Z";
    const OSTREAM_WRITE: &str =
        "?write@?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV12@PBDH@Z";
    const OSTREAM_ENDL: &str = "?endl@std@@YAAAV?$basic_ostream@DU?$char_traits@D@std@@@1@AAV21@@Z";
    const ISTREAM_READ: &str = "?read@?$basic_istream@DU?$char_traits@D@std@@@std@@QAEAAV12@PADH@Z";

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        p.vmem_next = 0x2000_0000;
        let stack = p.alloc_virtual(0x2000, PROT_RW);
        p.cpu.gpr[RSP] = stack + 0x800;
        p.wr32(p.cpu.gpr[RSP], 0).unwrap();
        p
    }

    fn args(p: &mut Process, values: &[u32]) {
        let rsp = p.cpu.gpr[RSP];
        for (index, value) in values.iter().enumerate() {
            p.wr32(rsp + 4 + index as u64 * 4, *value).unwrap();
        }
    }

    fn call(p: &mut Process, name: &str) -> Result<u64, Stop> {
        lookup(name).unwrap()(p)
    }

    #[test]
    fn all_installed_vc80_batch_imports_resolve() {
        for name in [
            "_atoflt",
            "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV01@H@Z",
            "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV01@P6AAAV01@AAV01@@Z@Z",
            "?length@?$char_traits@D@std@@SAIPBD@Z",
            "?sputc@?$basic_streambuf@DU?$char_traits@D@std@@@std@@QAEHD@Z",
            "?eof@?$char_traits@D@std@@SAHXZ",
            "?eq_int_type@?$char_traits@D@std@@SA_NABH0@Z",
            "?sputn@?$basic_streambuf@DU?$char_traits@D@std@@@std@@QAEHPBDH@Z",
            "?setstate@?$basic_ios@DU?$char_traits@D@std@@@std@@QAEXH_N@Z",
            "?_Osfx@?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEXXZ",
            "?_Unlock@_Mutex@std@@QAEXXZ",
            "?_Lock@_Mutex@std@@QAEXXZ",
            "?read@?$basic_istream@DU?$char_traits@D@std@@@std@@QAEAAV12@PADH@Z",
            "?write@?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV12@PBDH@Z",
            "?flush@?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV12@XZ",
            "?endl@std@@YAAAV?$basic_ostream@DU?$char_traits@D@std@@@1@AAV21@@Z",
        ] {
            assert!(lookup(name).is_some(), "missing HLE for {name}");
        }
    }

    #[test]
    fn atoflt_converts_float_and_reports_range_status() {
        let mut p = process();
        let input = p.alloc_virtual(0x1000, PROT_RW);
        let output = input + 0x100;
        for (text, expected_status, expected_value) in [
            (" -12.5tail", 0, -12.5f32),
            ("1e39", 3, f32::INFINITY),
            ("1e-46", 4, 0.0f32),
        ] {
            p.write_cstr(input, text).unwrap();
            args(&mut p, &[output as u32, input as u32]);
            assert_eq!(call(&mut p, "_atoflt"), Ok(expected_status));
            assert_eq!(p.rd32(output).unwrap(), expected_value.to_bits());
        }
        p.write_cstr(input, "nonsense").unwrap();
        args(&mut p, &[output as u32, input as u32]);
        assert_eq!(call(&mut p, "_atoflt"), Ok(0));
        assert_eq!(p.rd32(output).unwrap(), 0.0f32.to_bits());
    }

    #[test]
    fn pe32_ostream_and_streambuf_imports_write_real_output() {
        let mut p = process();
        let ostream = data_import(&mut p, MSVC_STD_COUT_SYMBOL, 0x80).unwrap();
        let streambuf = ostream + 0x40;
        p.cpu.gpr[RCX] = ostream;
        args(&mut p, &[73]);
        assert_eq!(call(&mut p, OSTREAM_INT), Ok(ostream));

        let data = p.alloc_virtual(0x1000, PROT_RW);
        p.write_bytes(data, b" + raw").unwrap();
        p.cpu.gpr[RCX] = streambuf;
        args(&mut p, &[data as u32, 6]);
        assert_eq!(call(&mut p, STREAMBUF_SPUTN), Ok(6));

        p.cpu.gpr[RCX] = ostream;
        args(&mut p, &[data as u32, 6]);
        assert_eq!(call(&mut p, OSTREAM_WRITE), Ok(ostream));

        p.cpu.gpr[RCX] = streambuf;
        args(&mut p, &[b'!'.into()]);
        assert_eq!(call(&mut p, STREAMBUF_SPUTC), Ok(b'!' as u64));

        p.cpu.gpr[RCX] = ostream;
        args(&mut p, &[ostream as u32]);
        assert_eq!(call(&mut p, OSTREAM_ENDL), Ok(ostream));
        assert_eq!(p.stdout, b"73 + raw + raw!\n");
    }

    #[test]
    fn pe32_istream_reads_from_process_stdin_and_sets_short_read_state() {
        let mut p = process();
        let istream = data_import(&mut p, MSVC_STD_CIN_SYMBOL, 0x80).unwrap();
        let output = p.alloc_virtual(0x1000, PROT_RW);
        p.stdin.extend([b'a', b'b']);
        p.cpu.gpr[RCX] = istream;
        args(&mut p, &[output as u32, 3]);
        assert_eq!(call(&mut p, ISTREAM_READ), Ok(istream));
        assert_eq!(p.read_bytes(output, 2).unwrap(), b"ab");
        assert_eq!(p.rd32(istream + 0x10 + 0x08).unwrap(), 0x3);
    }

    #[test]
    fn pe32_traits_setstate_flush_and_mutex_handlers_use_guest_state() {
        let mut p = process();
        let text = p.alloc_virtual(0x1000, PROT_RW);
        p.write_cstr(text, "stream").unwrap();
        args(&mut p, &[text as u32]);
        assert_eq!(call(&mut p, "?length@?$char_traits@D@std@@SAIPBD@Z"), Ok(6));
        assert_eq!(
            call(&mut p, "?eof@?$char_traits@D@std@@SAHXZ"),
            Ok(u32::MAX as u64)
        );
        let left = text + 0x100;
        let right = text + 0x104;
        p.wr32(left, 0x1234).unwrap();
        p.wr32(right, 0x1234).unwrap();
        args(&mut p, &[left as u32, right as u32]);
        assert_eq!(
            call(&mut p, "?eq_int_type@?$char_traits@D@std@@SA_NABH0@Z"),
            Ok(1)
        );

        let ostream = data_import(&mut p, MSVC_STD_COUT_SYMBOL, 0x80).unwrap();
        let ios = ostream + 0x10;
        p.cpu.gpr[RCX] = ios;
        args(&mut p, &[0x2, 0]);
        assert_eq!(
            call(
                &mut p,
                "?setstate@?$basic_ios@DU?$char_traits@D@std@@@std@@QAEXH_N@Z"
            ),
            Ok(0)
        );
        assert_eq!(p.rd32(ios + 8).unwrap(), 2);
        p.cpu.gpr[RCX] = ostream;
        assert_eq!(
            call(
                &mut p,
                "?flush@?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV12@XZ"
            ),
            Ok(ostream)
        );

        let cerr = data_import(
            &mut p,
            "?cerr@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A",
            0x80,
        )
        .unwrap();
        p.cpu.gpr[RCX] = cerr;
        assert_eq!(
            call(
                &mut p,
                "?_Osfx@?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEXXZ"
            ),
            Ok(0)
        );

        let mutex_this = p.alloc_virtual(0x1000, PROT_RW);
        let runtime_mutex = mutex_this + 0x100;
        p.wr32(mutex_this, runtime_mutex as u32).unwrap();
        p.cpu.gpr[RCX] = mutex_this;
        assert_eq!(call(&mut p, "?_Lock@_Mutex@std@@QAEXXZ"), Ok(0));
        assert!(p
            .critical_sections
            .get(&runtime_mutex)
            .unwrap()
            .owner
            .is_some());
        p.cpu.gpr[RCX] = mutex_this;
        assert_eq!(call(&mut p, "?_Unlock@_Mutex@std@@QAEXXZ"), Ok(0));
        assert_eq!(p.critical_sections.get(&runtime_mutex).unwrap().owner, None);
    }

    #[test]
    fn pe32_manipulator_insertion_calls_guest_function_with_ostream_reference() {
        let mut p = process();
        let ostream = data_import(&mut p, MSVC_STD_COUT_SYMBOL, 0x80).unwrap();
        let manipulator = p.alloc_virtual(0x1000, PROT_RWX);
        // `return *ostream;` for `ostream& manipulator(ostream&)`.
        p.write_bytes(manipulator, &[0x8b, 0x44, 0x24, 0x04, 0xc3])
            .unwrap();
        p.cpu.gpr[RCX] = ostream;
        args(&mut p, &[manipulator as u32]);
        assert_eq!(
            call(
                &mut p,
                "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV01@P6AAAV01@AAV01@@Z@Z",
            ),
            Ok(ostream)
        );
    }

    #[test]
    fn msvcr80_data_import_values_match_installed_exports() {
        let mut p = process();
        let huge = data_import(&mut p, "_HUGE", 8).unwrap();
        let adjust_fdiv = data_import(&mut p, "_adjust_fdiv", 4).unwrap();
        assert_eq!(p.rd64(huge).unwrap(), f64::INFINITY.to_bits());
        assert_eq!(p.rd32(adjust_fdiv).unwrap(), 0);
    }
}

#[cfg(test)]
mod onexit_tests {
    use super::{lookup, onexit_table_fields};
    use crate::process::{HleFn, Process, Stop};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RCX, RDX, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW, PROT_RWX};

    const CODE_BASE: u64 = 0x40000;
    const TABLE: u64 = CODE_BASE + 0x100;
    const ORDER: u64 = CODE_BASE + 0x200;
    const COUNT: u64 = CODE_BASE + 0x204;
    const FIRST_CALLBACK: u64 = CODE_BASE + 0x300;
    const SECOND_CALLBACK: u64 = CODE_BASE + 0x340;
    const COUNT_CALLBACK: u64 = CODE_BASE + 0x380;
    const STACK_BASE: u64 = 0x80000;
    const STACK: u64 = STACK_BASE + 0x800;

    fn append_order_callback(mode: CpuMode, target: u64, digit: u8) -> Vec<u8> {
        let mut code = if mode == CpuMode::X86_32 {
            vec![0xa1]
        } else {
            vec![0x48, 0xb8]
        };
        if mode == CpuMode::X86_32 {
            code.extend_from_slice(&(target as u32).to_le_bytes());
            code.extend_from_slice(&[0x6b, 0xc0, 10, 0x83, 0xc0, digit, 0xa3]);
            code.extend_from_slice(&(target as u32).to_le_bytes());
        } else {
            code.extend_from_slice(&target.to_le_bytes());
            code.extend_from_slice(&[0x8b, 0x08, 0x6b, 0xc9, 10, 0x83, 0xc1, digit, 0x89, 0x08]);
        }
        code.extend_from_slice(&[0x31, 0xc0, 0xc3]); // xor eax,eax; ret
        code
    }

    fn append_count_callback(mode: CpuMode, target: u64) -> Vec<u8> {
        let mut code = if mode == CpuMode::X86_32 {
            vec![0xa1]
        } else {
            vec![0x48, 0xb8]
        };
        if mode == CpuMode::X86_32 {
            code.extend_from_slice(&(target as u32).to_le_bytes());
            code.extend_from_slice(&[0x40, 0xa3]); // inc eax; mov [absolute],eax
            code.extend_from_slice(&(target as u32).to_le_bytes());
        } else {
            code.extend_from_slice(&target.to_le_bytes());
            code.extend_from_slice(&[0x8b, 0x08, 0xff, 0xc1, 0x89, 0x08]); // inc ecx; store
        }
        code.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        code
    }

    fn make_process(mode: CpuMode) -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(CODE_BASE, PAGE_SIZE, PROT_RWX));
        assert!(p.mem.map(STACK_BASE, PAGE_SIZE, PROT_RW));
        p.cpu.mode = mode;
        p.cpu.gpr[RSP] = STACK;
        p.write_bytes(FIRST_CALLBACK, &append_order_callback(mode, ORDER, 1))
            .unwrap();
        p.write_bytes(SECOND_CALLBACK, &append_order_callback(mode, ORDER, 2))
            .unwrap();
        p.write_bytes(COUNT_CALLBACK, &append_count_callback(mode, COUNT))
            .unwrap();
        p
    }

    fn call_hle(p: &mut Process, function: HleFn, args: &[u64]) -> Result<u64, Stop> {
        if p.cpu.mode == CpuMode::X86_32 {
            p.cpu.gpr[RSP] = STACK;
            p.wr32(STACK, 0)?;
            for (index, argument) in args.iter().enumerate() {
                p.wr32(STACK + 4 + index as u64 * 4, *argument as u32)?;
            }
        } else {
            p.cpu.gpr[RCX] = args.first().copied().unwrap_or(0);
            p.cpu.gpr[RDX] = args.get(1).copied().unwrap_or(0);
        }
        function(p)
    }

    fn onexit_table_lifecycle_uses_guest_width_and_reverse_callback_order(mode: CpuMode) {
        let mut p = make_process(mode);
        let initialize = lookup("_initialize_onexit_table").unwrap();
        let register = lookup("_register_onexit_function").unwrap();
        let execute = lookup("_execute_onexit_table").unwrap();
        let width = if mode == CpuMode::X86_32 { 4 } else { 8 };
        let table_sentinel = if width == 4 {
            u32::MAX as u64
        } else {
            u64::MAX
        };

        assert_eq!(call_hle(&mut p, initialize, &[TABLE]), Ok(0));
        assert_eq!(call_hle(&mut p, register, &[TABLE, FIRST_CALLBACK]), Ok(0));
        assert_eq!(call_hle(&mut p, register, &[TABLE, SECOND_CALLBACK]), Ok(0));
        assert_eq!(call_hle(&mut p, execute, &[TABLE]), Ok(0));
        assert_eq!(p.rd32(ORDER).unwrap(), 21);
        assert_eq!(onexit_table_fields(&p, TABLE).unwrap(), [table_sentinel; 3]);
        assert_eq!(call_hle(&mut p, execute, &[TABLE]), Ok(u64::MAX));
        assert_eq!(
            call_hle(&mut p, register, &[TABLE, FIRST_CALLBACK]),
            Ok(u64::MAX)
        );

        assert_eq!(call_hle(&mut p, initialize, &[TABLE]), Ok(0));
        p.wr32(COUNT, 0).unwrap();
        for _ in 0..40 {
            assert_eq!(call_hle(&mut p, register, &[TABLE, COUNT_CALLBACK]), Ok(0));
        }
        let [first, last, end] = onexit_table_fields(&p, TABLE).unwrap();
        assert_eq!(last - first, 40 * width);
        assert_eq!(
            end - first,
            64 * width,
            "the 33rd registration must grow the table"
        );
        assert_eq!(call_hle(&mut p, execute, &[TABLE]), Ok(0));
        assert_eq!(p.rd32(COUNT).unwrap(), 40);
        assert_eq!(
            p.heap.bytes_in_use, 0,
            "execution releases the grown callback table"
        );
    }

    #[test]
    fn pe32_onexit_table_lifecycle_handles_callbacks_and_growth() {
        onexit_table_lifecycle_uses_guest_width_and_reverse_callback_order(CpuMode::X86_32);
    }

    #[test]
    fn pe64_onexit_table_lifecycle_handles_callbacks_and_growth() {
        onexit_table_lifecycle_uses_guest_width_and_reverse_callback_order(CpuMode::X86_64);
    }
}

#[cfg(test)]
mod pointer_encode_tests {
    use super::lookup;
    use crate::process::Process;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RCX, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const STACK_BASE: u64 = 0x80000;
    const STACK: u64 = STACK_BASE + 0x800;

    fn call_pointer_api(name: &str, mode: CpuMode, value: u64) -> u64 {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(STACK_BASE, PAGE_SIZE, PROT_RW));
        p.cpu.mode = mode;
        p.cpu.gpr[RSP] = STACK;
        if mode == CpuMode::X86_32 {
            p.wr32(STACK + 4, value as u32).unwrap();
        } else {
            p.cpu.gpr[RCX] = value;
        }
        lookup(name).expect("pointer API should be available")(&mut p).unwrap()
    }

    #[test]
    fn encode_and_decode_pointer_preserve_guest_pointer_values() {
        for (mode, value) in [
            (CpuMode::X86_32, 0x1234_5678),
            (CpuMode::X86_64, 0x1234_5678_9abc_def0),
        ] {
            assert_eq!(call_pointer_api("_encode_pointer", mode, value), value);
            assert_eq!(call_pointer_api("_decode_pointer", mode, value), value);
            assert_eq!(call_pointer_api("_encode_pointer", mode, 0), 0);
            assert_eq!(call_pointer_api("_decode_pointer", mode, 0), 0);
        }
    }
}

#[cfg(test)]
mod crt_handler_audit_tests {
    use super::{crt_signal_cell, lookup};
    use crate::{
        hle::{crt, kernel32},
        process::{Process, Stop, TEB_ADDR},
        vfs::{FileMetadata, Vfs},
    };
    use anywasm_cpu::cpu::{CpuMode, R8, R9, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW, PROT_RWX};

    const STACK: u64 = 0x80000;
    const CODE: u64 = 0x90000;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(CODE, PAGE_SIZE, PROT_RWX));
        p.cpu.mode = CpuMode::X86_64;
        p.cpu.gpr[RSP] = STACK + 0x800;
        p
    }

    fn call(p: &mut Process, name: &str, args: &[u64]) -> Result<u64, Stop> {
        p.cpu.gpr[RCX] = args.first().copied().unwrap_or(0);
        p.cpu.gpr[RDX] = args.get(1).copied().unwrap_or(0);
        p.cpu.gpr[R8] = args.get(2).copied().unwrap_or(0);
        p.cpu.gpr[R9] = args.get(3).copied().unwrap_or(0);
        lookup(name).expect("CRT handler should be available")(p)
    }

    fn call_x86(p: &mut Process, name: &str, args: &[u64]) -> Result<u64, Stop> {
        p.cpu.mode = CpuMode::X86_32;
        let stack = STACK + 0x800;
        p.cpu.gpr[RSP] = stack;
        for (index, value) in args.iter().enumerate() {
            p.wr32(stack + 4 + index as u64 * 4, *value as u32)?;
        }
        lookup(name).expect("x86 CRT control API should be available")(p)
    }

    fn c_string(p: &mut Process, value: &str) -> u64 {
        let address = kernel32::alloc_block(p, value.len() as u64 + 1, true);
        p.write_cstr(address, value).unwrap();
        address
    }

    fn raw_c_string(p: &mut Process, value: &[u8]) -> u64 {
        let address = kernel32::alloc_block(p, value.len() as u64 + 1, true);
        p.write_bytes(address, &[value, &[0]].concat()).unwrap();
        address
    }

    #[test]
    fn x86_strcspn_counts_bytes_before_first_rejected_character() {
        let mut p = process();
        let text = raw_c_string(&mut p, b"abcdeabc");
        let reject = raw_c_string(&mut p, b"dx");
        let reject_none = raw_c_string(&mut p, b"");
        let high_bytes = raw_c_string(&mut p, &[0x80, b'a', 0xff, b'b']);
        let high_reject = raw_c_string(&mut p, &[0xff]);

        assert_eq!(call_x86(&mut p, "strcspn", &[text, reject]), Ok(3));
        assert_eq!(call_x86(&mut p, "strcspn", &[text, reject_none]), Ok(8));
        assert_eq!(
            call_x86(&mut p, "strcspn", &[high_bytes, high_reject]),
            Ok(2)
        );
    }

    #[test]
    fn x86_strpbrk_returns_first_matching_guest_byte_or_null() {
        let mut p = process();
        let text = raw_c_string(&mut p, &[b'a', 0x80, 0xff, b'b']);
        let accept = raw_c_string(&mut p, &[0xff, 0x80]);
        let accept_none = raw_c_string(&mut p, &[0xfe]);
        let accept_empty = raw_c_string(&mut p, b"");

        assert_eq!(call_x86(&mut p, "strpbrk", &[text, accept]), Ok(text + 1));
        assert_eq!(call_x86(&mut p, "strpbrk", &[text, accept_none]), Ok(0));
        assert_eq!(call_x86(&mut p, "strpbrk", &[text, accept_empty]), Ok(0));
    }

    #[test]
    fn wstat64i32_translates_real_file_size_permissions_drive_and_times() {
        let mut vfs = Vfs::new();
        vfs.set_root_alias(r"D:\The Sims 3").unwrap();
        let file = r"D:\The Sims 3\Game\Bin\TS3W.exe";
        vfs.insert(file, vec![1, 2, 3]);
        vfs.set_file_metadata(
            file,
            FileMetadata {
                accessed: Some(1_600_000_001),
                modified: Some(1_700_000_001),
                created: Some(1_500_000_001),
                mode: Some(0o100444),
                link_count: Some(2),
            },
        );
        let mut p = Process::new(vfs);
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(CODE, PAGE_SIZE, PROT_RWX));
        p.current_directory = r"D:\The Sims 3\Game\Bin".into();
        let path = kernel32::alloc_block(&mut p, 256, true);
        let output = kernel32::alloc_block(&mut p, 48, true);
        p.write_wstr(path, file).unwrap();

        assert_eq!(call_x86(&mut p, "_wstat64i32", &[path, output]), Ok(0));
        assert_eq!(p.rd32(output).unwrap(), 3); // D: drive number
        assert_eq!(p.rd16(output + 4).unwrap(), 0); // NTFS inode is undefined
        assert_eq!(p.rd16(output + 6).unwrap(), 0x8140); // regular, readable, executable
        assert_eq!(p.rd16(output + 8).unwrap(), 2);
        assert_eq!(p.rd16(output + 10).unwrap(), 0);
        assert_eq!(p.rd16(output + 12).unwrap(), 0);
        assert_eq!(p.rd32(output + 16).unwrap(), 3);
        assert_eq!(p.rd32(output + 20).unwrap(), 3);
        assert_eq!(p.rd64(output + 24).unwrap(), 1_600_000_001);
        assert_eq!(p.rd64(output + 32).unwrap(), 1_700_000_001);
        assert_eq!(p.rd64(output + 40).unwrap(), 1_500_000_001);
    }

    #[test]
    fn getdrive_reports_the_drive_letter_of_the_current_directory() {
        let mut p = process();
        p.current_directory = r"D:\The Sims 3\Game\Bin".into();
        assert_eq!(call_x86(&mut p, "_getdrive", &[]), Ok(4));

        p.current_directory = r"c:\Game".into();
        assert_eq!(call_x86(&mut p, "_getdrive", &[]), Ok(3));

        p.current_directory = r"\\server\share".into();
        assert_eq!(call_x86(&mut p, "_getdrive", &[]), Ok(0));
    }

    #[test]
    fn wstat64i32_returns_enoent_for_missing_paths_and_exposes_unknown_metadata() {
        let mut p = process();
        let path = kernel32::alloc_block(&mut p, 256, true);
        let output = kernel32::alloc_block(&mut p, 48, true);
        let errno = super::errno_cell(&mut p);
        p.write_wstr(path, r"C:\missing.dat").unwrap();
        assert_eq!(
            call_x86(&mut p, "_wstat64i32", &[path, output]),
            Ok(u32::MAX as u64)
        );
        assert_eq!(p.rd32(errno).unwrap(), 2); // ENOENT

        p.vfs.insert(r"C:\known.dat", vec![7]);
        p.write_wstr(path, r"C:\known.dat").unwrap();
        assert_eq!(
            call_x86(&mut p, "_wstat64i32", &[path, output]),
            Err(Stop::Unimplemented {
                dll: "crt".into(),
                name: "_wstat64i32".into()
            })
        );
        assert!(String::from_utf8_lossy(&p.stderr).contains("[coverage-gap] crt!_wstat64i32"));
        assert!(String::from_utf8_lossy(&p.stderr).contains("st_atime"));
    }

    #[test]
    fn x86_setjmp3_saves_the_msvc_jmp_buffer_and_current_seh_state() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.gpr[RBX] = 0x1111_2222;
        p.cpu.gpr[RBP] = 0x3333_4444;
        p.cpu.gpr[RDI] = 0x5555_6666;
        p.cpu.gpr[RSI] = 0x7777_8888;
        p.wr32(TEB_ADDR, u32::MAX).unwrap();
        let buffer = kernel32::alloc_block(&mut p, 64, true);
        let stack = STACK + 0x800;
        p.wr32(stack, 0x0049_62a9).unwrap();

        assert_eq!(call_x86(&mut p, "_setjmp3", &[buffer, 0]), Ok(0));
        assert_eq!(p.rd32(buffer).unwrap(), 0x3333_4444); // Ebp
        assert_eq!(p.rd32(buffer + 4).unwrap(), 0x1111_2222); // Ebx
        assert_eq!(p.rd32(buffer + 8).unwrap(), 0x5555_6666); // Edi
        assert_eq!(p.rd32(buffer + 12).unwrap(), 0x7777_8888); // Esi
        assert_eq!(p.rd32(buffer + 16).unwrap(), (stack + 4) as u32); // Esp after call
        assert_eq!(p.rd32(buffer + 20).unwrap(), 0x0049_62a9); // Eip after call
        assert_eq!(p.rd32(buffer + 24).unwrap(), u32::MAX); // TEB.ExceptionList
        assert_eq!(p.rd32(buffer + 28).unwrap(), u32::MAX); // TRYLEVEL_END
        assert_eq!(p.rd32(buffer + 32).unwrap(), 0x5643_3230); // VC20 jump-buffer cookie
        assert_eq!(p.rd32(buffer + 36).unwrap(), 0); // no optional unwind callback
        for index in 0..6 {
            assert_eq!(p.rd32(buffer + 40 + index * 4).unwrap(), 0);
        }
    }

    #[test]
    fn x86_longjmp_restores_the_msvc_state_and_normalizes_zero_to_one() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.gpr[RBX] = 0x1122_3344;
        p.cpu.gpr[RBP] = 0x5566_7788;
        p.cpu.gpr[RDI] = 0x99aa_bbcc;
        p.cpu.gpr[RSI] = 0xddee_ff00;
        p.wr32(TEB_ADDR, u32::MAX).unwrap();
        let buffer = kernel32::alloc_block(&mut p, 64, true);
        let stack = STACK + 0x800;
        p.wr32(stack, 0x0049_62a9).unwrap();
        assert_eq!(call_x86(&mut p, "_setjmp3", &[buffer, 0]), Ok(0));

        p.cpu.gpr[RBX] = 0;
        p.cpu.gpr[RBP] = 0;
        p.cpu.gpr[RDI] = 0;
        p.cpu.gpr[RSI] = 0;
        p.wr32(stack, 0x0049_7000).unwrap();
        assert_eq!(call_x86(&mut p, "longjmp", &[buffer, 0]), Ok(0));

        assert_eq!(p.cpu.rip, 0x0049_62a9);
        assert_eq!(p.cpu.gpr[RSP], stack + 4);
        assert_eq!(p.cpu.gpr[RBP], 0x5566_7788);
        assert_eq!(p.cpu.gpr[RBX], 0x1122_3344);
        assert_eq!(p.cpu.gpr[RDI], 0x99aa_bbcc);
        assert_eq!(p.cpu.gpr[RSI], 0xddee_ff00);
        assert_eq!(p.cpu.gpr[RAX], 1);
        assert!(p.skip_ret);
    }

    #[test]
    fn x86_setjmp3_and_longjmp_expose_untranslated_unwind_requirements() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.wr32(TEB_ADDR, u32::MAX).unwrap();
        let buffer = kernel32::alloc_block(&mut p, 64, true);
        let stack = STACK + 0x800;
        p.wr32(stack, 0x0049_62a9).unwrap();
        assert!(matches!(
            call_x86(&mut p, "_setjmp3", &[buffer, 1, 0x0049_9000]),
            Err(Stop::Unimplemented { dll, name }) if dll == "crt" && name == "_setjmp3"
        ));
        assert!(String::from_utf8_lossy(&p.stderr).contains("optional unwind arguments"));

        p.wr32(stack, 0x0049_62a9).unwrap();
        assert_eq!(call_x86(&mut p, "_setjmp3", &[buffer, 0]), Ok(0));
        p.wr32(TEB_ADDR, 0x0008_1000).unwrap();
        assert!(matches!(
            call_x86(&mut p, "longjmp", &[buffer, 1]),
            Err(Stop::Unimplemented { dll, name }) if dll == "crt" && name == "longjmp"
        ));
        assert!(String::from_utf8_lossy(&p.stderr)
            .contains("[coverage-gap] crt!longjmp requires untranslated unwind state"));
    }

    fn signal_handler(p: &mut Process, address: u64, signal_out: u64, subcode_out: u64) {
        // Record the C-decl callback arguments from [esp+4] and [esp+8], then return.
        let mut code = vec![0x8b, 0x44, 0x24, 0x04, 0xa3];
        code.extend_from_slice(&(signal_out as u32).to_le_bytes());
        code.extend_from_slice(&[0x8b, 0x44, 0x24, 0x08, 0xa3]);
        code.extend_from_slice(&(subcode_out as u32).to_le_bytes());
        code.push(0xc3);
        p.write_bytes(address, &code).unwrap();
    }

    fn exception_pointers(p: &mut Process, address: u64) -> u64 {
        let exception_record = address + 0x20;
        let context_record = address + 0x80;
        p.wr32(address, exception_record as u32).unwrap();
        p.wr32(address + 4, context_record as u32).unwrap();
        p.wr32(exception_record, 0xc000_0005).unwrap();
        p.wr32(exception_record + 4, 0).unwrap();
        address
    }

    #[test]
    fn putenv_updates_deletes_and_rejects_invalid_assignments() {
        let mut p = process();
        assert!(p
            .env
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("path")));
        let original_len = p.env.len();
        let update = c_string(&mut p, "path=new");
        assert_eq!(call(&mut p, "_putenv", &[update]), Ok(0));
        let path_entry = p
            .env
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
            .unwrap();
        assert_eq!(path_entry.1, "new");
        assert_eq!(
            p.env.len(),
            original_len,
            "the case-insensitive match is replaced in place"
        );

        let delete = c_string(&mut p, "Path=");
        assert_eq!(call(&mut p, "_putenv", &[delete]), Ok(0));
        assert!(!p
            .env
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("path")));

        let invalid = c_string(&mut p, "missing-separator");
        assert_eq!(call(&mut p, "_putenv", &[invalid]), Ok(u64::MAX));
        let errno = p.extra.errno_cell;
        assert_ne!(errno, 0);
        assert_eq!(p.rd32(errno).unwrap(), 22);
    }

    #[test]
    fn wgetcwd_writes_the_full_guest_directory_and_can_allocate_the_return_buffer() {
        let mut p = process();
        p.current_directory = r"C:\Game\Bin".into();
        let buffer = kernel32::alloc_block(&mut p, 64, true);

        assert_eq!(call_x86(&mut p, "_wgetcwd", &[buffer, 12]), Ok(buffer));
        assert_eq!(p.read_wstr(buffer).unwrap(), r"C:\Game\Bin");

        let narrow = kernel32::alloc_block(&mut p, 32, true);
        assert_eq!(call_x86(&mut p, "_getcwd", &[narrow, 12]), Ok(narrow));
        assert_eq!(p.read_cstr(narrow).unwrap(), r"C:\Game\Bin");

        let allocated = call_x86(&mut p, "_wgetcwd", &[0, 12]).unwrap();
        assert_ne!(allocated, 0);
        assert_eq!(p.read_wstr(allocated).unwrap(), r"C:\Game\Bin");
        assert!(kernel32::free_block(&mut p, allocated));

        p.current_directory = r"C:\".into();
        assert_eq!(call_x86(&mut p, "_wgetcwd", &[buffer, 4]), Ok(buffer));
        assert_eq!(p.read_wstr(buffer).unwrap(), r"C:\");
    }

    #[test]
    fn wgetdcwd_returns_the_real_current_drive_directory_for_zero_and_matching_drive() {
        let mut p = process();
        p.current_directory = r"D:\The Sims 3\Game\Bin".into();
        let buffer = kernel32::alloc_block(&mut p, 128, true);

        assert_eq!(call_x86(&mut p, "_wgetdcwd", &[0, buffer, 32]), Ok(buffer));
        assert_eq!(p.read_wstr(buffer).unwrap(), r"D:\The Sims 3\Game\Bin");

        assert_eq!(call_x86(&mut p, "_wgetdcwd", &[4, buffer, 32]), Ok(buffer));
        assert_eq!(p.read_wstr(buffer).unwrap(), r"D:\The Sims 3\Game\Bin");

        let narrow = kernel32::alloc_block(&mut p, 128, true);
        assert_eq!(call_x86(&mut p, "_getdcwd", &[4, narrow, 128]), Ok(narrow));
        assert_eq!(p.read_cstr(narrow).unwrap(), r"D:\The Sims 3\Game\Bin");
    }

    #[test]
    fn wgetdcwd_rejects_a_drive_whose_current_directory_is_not_modeled() {
        let mut p = process();
        p.current_directory = r"D:\The Sims 3\Game\Bin".into();
        let buffer = kernel32::alloc_block(&mut p, 128, true);
        let errno = super::errno_cell(&mut p);
        p.write_bytes(CODE, &[0xc3]).unwrap();
        p.extra.invalid_parameter_handler = CODE;

        assert_eq!(call_x86(&mut p, "_wgetdcwd", &[3, buffer, 32]), Ok(0));
        assert_eq!(p.rd32(errno).unwrap(), 22); // invalid drive invokes parameter validation
    }

    #[test]
    fn wgetcwd_reports_range_and_invalid_parameter_errors_instead_of_faking_a_path() {
        let mut p = process();
        p.current_directory = r"C:\Game\Bin".into();
        let buffer = kernel32::alloc_block(&mut p, 64, true);
        let errno = super::errno_cell(&mut p);

        assert_eq!(call_x86(&mut p, "_wgetcwd", &[buffer, 4]), Ok(0));
        assert_eq!(p.rd32(errno).unwrap(), 34); // ERANGE

        p.write_bytes(CODE, &[0xc3]).unwrap();
        p.extra.invalid_parameter_handler = CODE;
        assert_eq!(call_x86(&mut p, "_wgetcwd", &[buffer, 0]), Ok(0));
        assert_eq!(p.rd32(errno).unwrap(), 22); // EINVAL after the handler returns
    }

    #[test]
    fn wtol_parses_signed_decimal_prefixes_and_saturates_32_bit_long_overflow() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let input = kernel32::alloc_block(&mut p, 128, true);
        let errno = super::errno_cell(&mut p);

        p.write_wstr(input, " \t-2147483648tail").unwrap();
        assert_eq!(
            call_x86(&mut p, "_wtol", &[input]),
            Ok(i32::MIN as u32 as u64)
        );
        p.write_wstr(input, "+2147483647suffix").unwrap();
        assert_eq!(
            call_x86(&mut p, "_wtol", &[input]),
            Ok(i32::MAX as u32 as u64)
        );

        p.wr32(errno, 0x1234).unwrap();
        p.write_wstr(input, "not a number").unwrap();
        assert_eq!(call_x86(&mut p, "_wtol", &[input]), Ok(0));
        assert_eq!(
            p.rd32(errno).unwrap(),
            0x1234,
            "no conversion leaves errno alone"
        );

        p.write_wstr(input, "2147483648").unwrap();
        assert_eq!(
            call_x86(&mut p, "_wtol", &[input]),
            Ok(i32::MAX as u32 as u64)
        );
        assert_eq!(p.rd32(errno).unwrap(), 34); // ERANGE
        p.write_wstr(input, "-2147483649").unwrap();
        assert_eq!(
            call_x86(&mut p, "_wtol", &[input]),
            Ok(i32::MIN as u32 as u64)
        );
        assert_eq!(p.rd32(errno).unwrap(), 34); // ERANGE
    }

    #[test]
    fn wtol_null_input_uses_the_invalid_parameter_handler() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        assert!(matches!(
            call_x86(&mut p, "_wtol", &[0]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("_wtol") && name.contains("invalid-parameter")
        ));

        p.write_bytes(CODE, &[0xc3]).unwrap();
        p.extra.invalid_parameter_handler = CODE;
        assert_eq!(call_x86(&mut p, "_wtol", &[0]), Ok(0));
        let errno = super::errno_cell(&mut p);
        assert_eq!(p.rd32(errno).unwrap(), 22); // EINVAL
    }

    #[test]
    fn fflush_clears_input_pushback_and_flushall_reports_stream_count() {
        let mut p = process();
        let first = call(&mut p, "_flushall", &[]).unwrap();
        assert_eq!(first, 3, "the three standard streams are open");
        let stdin = p.extra.iob;
        p.extra.files.get_mut(&stdin).unwrap().ungot = Some(b'x');
        assert_eq!(call(&mut p, "fflush", &[stdin]), Ok(0));
        assert_eq!(p.extra.files[&stdin].ungot, None);
        assert_eq!(call(&mut p, "fflush", &[0]), Ok(0));

        assert_eq!(call(&mut p, "fflush", &[0x1234]), Ok(u64::MAX));
        assert_eq!(p.rd32(p.extra.errno_cell).unwrap(), 22);
    }

    #[test]
    fn crt_locks_are_recursive_and_file_lock_is_distinct() {
        let mut p = process();
        let lock_id = 5;
        assert_eq!(call(&mut p, "_lock", &[lock_id]), Ok(0));
        assert_eq!(call(&mut p, "_lock", &[lock_id]), Ok(0));
        assert_eq!(call(&mut p, "_unlock", &[lock_id]), Ok(0));
        assert_eq!(call(&mut p, "_unlock", &[lock_id]), Ok(0));
        assert_eq!(call(&mut p, "_mlock", &[lock_id]), Ok(0));
        assert_eq!(call(&mut p, "_munlock", &[lock_id]), Ok(0));

        call(&mut p, "_flushall", &[]).unwrap();
        let stdout = p.extra.iob + 0x30;
        assert_eq!(call(&mut p, "_lock_file", &[stdout]), Ok(0));
        assert_eq!(call(&mut p, "_unlock_file", &[stdout]), Ok(0));
        assert!(p
            .critical_sections
            .values()
            .all(|state| state.owner.is_none()));
    }

    #[test]
    fn locale_name_reports_the_only_supported_locale() {
        let mut p = process();
        let name = call(&mut p, "___lc_locale_name_func", &[]).unwrap();
        assert_eq!(p.read_cstr(name).unwrap(), "C");
        assert_eq!(call(&mut p, "___lc_locale_name_func", &[]), Ok(name));
    }

    #[test]
    fn fpreset_restores_the_x87_default_state() {
        let mut p = process();
        p.cpu.fpu_st = [3.5; 8];
        p.cpu.fpu_top = 4;
        p.cpu.fpu_cw = 0;
        p.cpu.fpu_sw = 0xffff;
        p.cpu.fpu_tw = 0;
        assert_eq!(call(&mut p, "_fpreset", &[]), Ok(0));
        assert_eq!(p.cpu.fpu_st, [0.0; 8]);
        assert_eq!(p.cpu.fpu_top, 0);
        assert_eq!(p.cpu.fpu_cw, 0x037f);
        assert_eq!(p.cpu.fpu_sw, 0);
        assert_eq!(p.cpu.fpu_tw, 0xffff);
    }

    #[test]
    fn controlfp_s_queries_and_updates_x87_and_sse_control_words() {
        let mut p = process();
        p.cpu.fpu_cw = 0x027f; // MSVC's default 53-bit x87 precision and masked exceptions.
        let output = kernel32::alloc_block(&mut p, 4, true);

        assert_eq!(call_x86(&mut p, "_controlfp_s", &[output, 0, 0]), Ok(0));
        assert_eq!(p.rd32(output).unwrap(), 0x0009_001f);

        assert_eq!(
            call_x86(&mut p, "_controlfp_s", &[output, 0x0002_0000, 0x0003_0000]),
            Ok(0)
        );
        assert_eq!(p.cpu.fpu_cw & 0x0300, 0);
        assert_eq!(p.rd32(output).unwrap(), 0x000a_001f);

        assert_eq!(
            call_x86(&mut p, "_controlfp_s", &[output, 0x200, 0x300]),
            Ok(0)
        );
        assert_eq!(p.cpu.fpu_cw & 0x0c00, 0x0800);
        assert_eq!(p.cpu.mxcsr & 0x6000, 0x4000);
        assert_eq!(p.rd32(output).unwrap() & 0x300, 0x200);

        // _controlfp_s leaves the denormal exception mask unchanged.
        assert_eq!(
            call_x86(&mut p, "_controlfp_s", &[output, 0, 0x0008_001f]),
            Ok(0)
        );
        assert_eq!(p.cpu.fpu_cw & (1 << 1), 1 << 1);
        assert_eq!(p.cpu.mxcsr & (1 << 8), 1 << 8);
        assert_eq!(p.cpu.fpu_cw & 1, 0);
        assert_eq!(p.cpu.mxcsr & (1 << 7), 0);
    }

    #[test]
    fn controlfp_s_applies_a_valid_update_when_output_pointer_is_null() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK + 0x800;
        let original_mxcsr = p.cpu.mxcsr;

        assert_eq!(
            call_x86(&mut p, "_controlfp_s", &[0, 0x0001_0000, 0x0003_0000]),
            Ok(0)
        );
        assert_eq!(p.cpu.fpu_cw & 0x0300, 0x0200);
        assert_eq!(p.cpu.mxcsr, original_mxcsr);
    }

    #[test]
    fn controlfp_families_validate_secure_outputs_and_denormal_semantics() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK + 0x800;

        assert_eq!(
            call_x86(&mut p, "_controlfp_s", &[0, 0x0003_0000, 0x0003_0000]),
            Ok(22)
        );
        assert_eq!(p.rd32(p.extra.errno_cell).unwrap(), 22);
        let before = (p.cpu.fpu_cw, p.cpu.mxcsr);
        let output = kernel32::alloc_block(&mut p, 4, true);
        assert_eq!(
            call_x86(&mut p, "_controlfp_s", &[output, 0x0003_0000, 0x0003_0000]),
            Ok(22)
        );
        assert_eq!((p.cpu.fpu_cw, p.cpu.mxcsr), before);

        assert_eq!(
            call_x86(&mut p, "_controlfp", &[0, 0x0008_0000]),
            Ok(0x0008_001f)
        );
        assert_eq!(p.cpu.fpu_cw & (1 << 1), 1 << 1);
        assert_eq!(p.cpu.mxcsr & (1 << 8), 1 << 8);
        assert_eq!(
            call_x86(&mut p, "_control87", &[0, 0x0008_0000]),
            Ok(0x0000_001f)
        );
        assert_eq!(p.cpu.fpu_cw & (1 << 1), 0);
        assert_eq!(p.cpu.mxcsr & (1 << 8), 0);
    }

    #[test]
    fn signal_returns_previous_handler_and_raise_invokes_then_resets_it() {
        let mut p = process();
        let handler = CODE + 0x100;
        p.write_bytes(handler, &[0xb8, 42, 0, 0, 0, 0xc3]).unwrap();
        assert_eq!(call(&mut p, "signal", &[2, handler]), Ok(0));
        assert_eq!(call(&mut p, "signal", &[2, 1]), Ok(handler));
        assert_eq!(call(&mut p, "signal", &[2, handler]), Ok(1));
        assert_eq!(call(&mut p, "raise", &[2]), Ok(0));
        assert_eq!(call(&mut p, "signal", &[2, 0]), Ok(0));

        assert_eq!(call(&mut p, "signal", &[8, handler]), Ok(0));
        assert_eq!(call(&mut p, "raise", &[8]), Ok(0));
        assert_eq!(call(&mut p, "signal", &[8, 1]), Ok(handler));
    }

    #[test]
    fn signal_errors_set_errno_and_default_handler_exits() {
        let mut p = process();
        assert_eq!(call(&mut p, "signal", &[3, 0]), Ok(u32::MAX as u64));
        assert_eq!(p.rd32(p.extra.errno_cell).unwrap(), 22);
        assert_eq!(
            call(&mut p, "signal", &[2, u32::MAX as u64]),
            Ok(u32::MAX as u64)
        );
        assert_eq!(call(&mut p, "raise", &[3]), Ok(1));
        assert_eq!(call(&mut p, "signal", &[2, 0]), Ok(0));
        assert_eq!(call(&mut p, "raise", &[2]), Err(Stop::Exit(3)));
    }

    #[test]
    fn global_invalid_parameter_handler_registration_returns_previous_handler() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let first = 0x0012_3456;
        let second = 0x0065_4321;

        assert_eq!(
            call_x86(&mut p, "_get_invalid_parameter_handler", &[]),
            Ok(0)
        );
        assert_eq!(
            call_x86(&mut p, "_set_invalid_parameter_handler", &[first]),
            Ok(0)
        );
        assert_eq!(
            call_x86(&mut p, "_get_invalid_parameter_handler", &[]),
            Ok(first)
        );
        assert_eq!(
            call_x86(&mut p, "_set_invalid_parameter_handler", &[second]),
            Ok(first)
        );
        assert_eq!(
            call_x86(&mut p, "_set_invalid_parameter_handler", &[0]),
            Ok(second)
        );
        assert_eq!(
            call_x86(&mut p, "_get_invalid_parameter_handler", &[]),
            Ok(0)
        );
    }

    #[test]
    fn invalid_parameter_handler_is_called_with_retail_arguments_before_recoverable_errors() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let count_cell = CODE + 0x100;
        let arguments = CODE + 0x110;
        let handler = CODE + 0x300;

        // __cdecl handler: increment a counter, copy its five stack arguments, and return.
        let mut code = vec![0xa1]; // mov eax, [count_cell]
        code.extend_from_slice(&(count_cell as u32).to_le_bytes());
        code.extend_from_slice(&[0x40, 0xa3]); // inc eax; mov [count_cell], eax
        code.extend_from_slice(&(count_cell as u32).to_le_bytes());
        for index in 0..5u8 {
            code.extend_from_slice(&[0x8b, 0x44, 0x24, 4 + index * 4]); // mov eax, [esp+arg]
            code.push(0xa3); // mov [arguments + index*4], eax
            code.extend_from_slice(&((arguments + index as u64 * 4) as u32).to_le_bytes());
        }
        code.extend_from_slice(&[0x31, 0xc0, 0xc3]); // xor eax,eax; ret
        p.write_bytes(handler, &code).unwrap();
        assert_eq!(
            call_x86(&mut p, "_set_invalid_parameter_handler", &[handler]),
            Ok(0)
        );

        let destination = kernel32::alloc_block(&mut p, 16, true);
        let source = kernel32::alloc_block(&mut p, 16, true);
        assert_eq!(
            call_x86(&mut p, "wcsncpy", &[destination, source, 0]),
            Ok(u32::MAX as u64)
        );
        assert_eq!(
            call_x86(&mut p, "_wcsicmp", &[0, source]),
            Ok(i32::MAX as u32 as u64)
        );
        assert_eq!(p.rd32(count_cell).unwrap(), 2);
        for index in 0..5 {
            assert_eq!(p.rd32(arguments + index * 4).unwrap(), 0);
        }
        assert_eq!(p.rd32(p.extra.errno_cell).unwrap(), 22);
    }

    #[test]
    fn default_invalid_parameter_watson_path_remains_explicitly_untranslated() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let destination = kernel32::alloc_block(&mut p, 16, true);
        let source = kernel32::alloc_block(&mut p, 16, true);

        assert!(matches!(
            call_x86(&mut p, "wcsncpy", &[destination, source, 0]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("Watson")
        ));
    }

    #[test]
    fn set_abort_behavior_returns_old_flags_and_updates_only_masked_bits() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;

        assert_eq!(call_x86(&mut p, "_set_abort_behavior", &[0, 3]), Ok(3));
        assert_eq!(call_x86(&mut p, "_set_abort_behavior", &[1, 1]), Ok(0));
        assert_eq!(call_x86(&mut p, "_set_abort_behavior", &[2, 2]), Ok(1));
        assert_eq!(call_x86(&mut p, "_set_abort_behavior", &[0, 3]), Ok(3));
    }

    #[test]
    fn abort_uses_registered_flags_and_exposes_untranslated_reportfault() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        assert_eq!(call_x86(&mut p, "_set_abort_behavior", &[0, 3]), Ok(3));
        assert!(matches!(call_x86(&mut p, "abort", &[]), Err(Stop::Exit(3))));
        assert!(
            p.stderr.is_empty(),
            "clearing WRITE_ABORT_MSG suppresses CRT text"
        );

        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        assert_eq!(call_x86(&mut p, "_set_abort_behavior", &[2, 3]), Ok(3));
        assert!(matches!(
            call_x86(&mut p, "abort", &[]),
            Err(Stop::Crash(message)) if message.contains("untranslated") && message.contains("Windows Error Reporting")
        ));
    }

    #[test]
    fn abort_does_not_emit_an_unverified_runtime_message() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;

        assert!(matches!(
            call_x86(&mut p, "abort", &[]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("abort message")
        ));
        assert!(p.stderr.is_empty());
    }

    #[test]
    fn abort_calls_the_registered_sigabrt_handler_before_terminating() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let marker = CODE + 0x100;
        let handler = CODE + 0x200;
        let mut code = vec![0xc7, 0x05]; // mov dword ptr [absolute address], immediate
        code.extend_from_slice(&(marker as u32).to_le_bytes());
        code.extend_from_slice(&0x51ab_cdefu32.to_le_bytes());
        code.push(0xc3);
        p.write_bytes(handler, &code).unwrap();

        assert_eq!(call_x86(&mut p, "signal", &[22, handler]), Ok(0));
        assert_eq!(call_x86(&mut p, "_set_abort_behavior", &[0, 3]), Ok(3));
        assert!(matches!(call_x86(&mut p, "abort", &[]), Err(Stop::Exit(3))));
        assert_eq!(p.rd32(marker).unwrap(), 0x51ab_cdef);
        let signal_cell = crt_signal_cell(&mut p, 22);
        assert_eq!(p.rdptr(signal_cell).unwrap(), 0);
    }

    #[test]
    fn xcpt_filter_dispatches_access_violation_to_one_shot_sigsegv_handler() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let handler = CODE + 0x100;
        let signal_out = CODE + 0x300;
        let subcode_out = CODE + 0x304;
        let pointers = exception_pointers(&mut p, CODE + 0x400);
        signal_handler(&mut p, handler, signal_out, subcode_out);

        assert_eq!(call_x86(&mut p, "signal", &[11, handler]), Ok(0));
        assert_eq!(
            call_x86(&mut p, "_XcptFilter", &[0xc000_0005, pointers]),
            Ok(u32::MAX as u64),
            "a returning SIGSEGV handler resumes the faulting context"
        );
        assert_eq!(p.rd32(signal_out).unwrap(), 11);
        let signal_cell = crt_signal_cell(&mut p, 11);
        assert_eq!(p.rdptr(signal_cell).unwrap(), 0, "SIGSEGV is one-shot");
    }

    #[test]
    fn xcpt_filter_maps_illegal_and_privileged_instructions_to_sigill() {
        for code in [0xc000_001du32, 0xc000_0096u32] {
            let mut p = process();
            p.cpu.mode = CpuMode::X86_32;
            let handler = CODE + 0x100;
            let signal_out = CODE + 0x300;
            let subcode_out = CODE + 0x304;
            let pointers = exception_pointers(&mut p, CODE + 0x400);
            signal_handler(&mut p, handler, signal_out, subcode_out);
            assert_eq!(call_x86(&mut p, "signal", &[4, handler]), Ok(0));

            assert_eq!(
                call_x86(&mut p, "_XcptFilter", &[code as u64, pointers]),
                Ok(u32::MAX as u64),
                "mapped SIGILL returns EXCEPTION_CONTINUE_EXECUTION"
            );
            assert_eq!(p.rd32(signal_out).unwrap(), 4);
            let signal_cell = crt_signal_cell(&mut p, 4);
            assert_eq!(p.rdptr(signal_cell).unwrap(), 0);
        }
    }

    #[test]
    fn xcpt_filter_maps_float_statuses_to_sigfpe_with_msvc_subcodes() {
        let cases = [
            (0xc000_008du32, 0x82u32), // _FPE_DENORMAL
            (0xc000_008eu32, 0x83u32), // _FPE_ZERODIVIDE
            (0xc000_008fu32, 0x86u32), // _FPE_INEXACT
            (0xc000_0090u32, 0x81u32), // _FPE_INVALID
            (0xc000_0091u32, 0x84u32), // _FPE_OVERFLOW
            (0xc000_0092u32, 0x8au32), // _FPE_STACKOVERFLOW
            (0xc000_0093u32, 0x85u32), // _FPE_UNDERFLOW
        ];
        for (code, subcode) in cases {
            let mut p = process();
            p.cpu.mode = CpuMode::X86_32;
            let handler = CODE + 0x100;
            let signal_out = CODE + 0x300;
            let subcode_out = CODE + 0x304;
            let pointers = exception_pointers(&mut p, CODE + 0x400);
            signal_handler(&mut p, handler, signal_out, subcode_out);
            assert_eq!(call_x86(&mut p, "signal", &[8, handler]), Ok(0));

            assert_eq!(
                call_x86(&mut p, "_XcptFilter", &[code as u64, pointers]),
                Ok(u32::MAX as u64),
                "mapped SIGFPE returns EXCEPTION_CONTINUE_EXECUTION"
            );
            assert_eq!(p.rd32(signal_out).unwrap(), 8);
            assert_eq!(p.rd32(subcode_out).unwrap(), subcode);
            let signal_cell = crt_signal_cell(&mut p, 8);
            assert_eq!(p.rdptr(signal_cell).unwrap(), handler);
        }
    }

    #[test]
    fn xcpt_filter_ignored_signal_continues_execution_and_default_delegates() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let pointers = exception_pointers(&mut p, CODE + 0x400);
        assert_eq!(call_x86(&mut p, "signal", &[11, 1]), Ok(0));
        assert_eq!(
            call_x86(&mut p, "_XcptFilter", &[0xc000_0005, pointers]),
            Ok(u32::MAX as u64),
            "SIG_IGN is EXCEPTION_CONTINUE_EXECUTION"
        );

        assert_eq!(call_x86(&mut p, "signal", &[11, 0]), Ok(1));
        let default_filter = CODE + 0x200;
        let observed_pointers = CODE + 0x308;
        // Record the exception-pointers argument and choose EXCEPTION_EXECUTE_HANDLER.
        let mut code = vec![0x8b, 0x44, 0x24, 0x04, 0xa3];
        code.extend_from_slice(&(observed_pointers as u32).to_le_bytes());
        code.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xc3]);
        p.write_bytes(default_filter, &code).unwrap();
        p.top_level_exception_filter = default_filter;
        assert_eq!(
            call_x86(&mut p, "_XcptFilter", &[0xc000_0005, pointers]),
            Ok(1),
            "SIG_DFL delegates to the process top-level exception filter"
        );
        assert_eq!(p.rd32(observed_pointers).unwrap(), pointers as u32);
    }

    #[test]
    fn xcpt_filter_default_and_unmapped_statuses_execute_the_process_default_handler() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let pointers = exception_pointers(&mut p, CODE + 0x400);
        assert_eq!(
            call_x86(&mut p, "_XcptFilter", &[0xc000_0005, pointers]),
            Ok(1),
            "default SIGSEGV delegates to UnhandledExceptionFilter, whose no-debugger default is EXECUTE_HANDLER"
        );
        assert_eq!(
            call_x86(&mut p, "_XcptFilter", &[0xe123_4567, pointers]),
            Ok(1),
            "unknown exception codes use the same default filter path"
        );
    }

    #[test]
    fn xcpt_filter_invalid_exception_pointers_are_reported_as_memory_faults() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        assert!(
            matches!(
                call_x86(&mut p, "_XcptFilter", &[0xe123_4567, CODE + PAGE_SIZE]),
                Err(Stop::Crash(_))
            ),
            "invalid guest exception pointers must remain visible"
        );
    }

    #[test]
    fn unsupported_constant_or_noop_crt_names_remain_unresolved() {
        for name in [
            "system",
            "setbuf",
            "_register_thread_local_exe_atexit_callback",
            "_set_fmode",
            "_set_new_mode",
            "_set_error_mode",
            "_crt_at_quick_exit",
            "_initialize_narrow_environment",
            "_initialize_wide_environment",
            "__security_init_cookie",
            "_seh_filter_exe",
            "_clearfp",
            "_statusfp",
            "__lconv_init",
            "_set_thread_local_invalid_parameter_handler",
            "_CrtSetReportMode",
            "_CrtSetReportFile",
            "_invalid_parameter_noinfo",
            "_invalid_parameter_noinfo_noreturn",
        ] {
            assert!(lookup(name).is_none(), "{name} should remain unresolved");
        }
        assert!(lookup("___mb_cur_max_func").is_some());
        assert!(lookup("_set_invalid_parameter_handler").is_some());
        assert!(lookup("_get_invalid_parameter_handler").is_some());
        assert!(lookup("_set_abort_behavior").is_some());
        assert!(lookup("_controlfp_s").is_some());
        assert!(lookup("_controlfp").is_some());
        assert!(lookup("_control87").is_some());
    }

    #[test]
    fn set_app_type_records_process_startup_mode() {
        let mut p = process();
        assert_eq!(p.crt_app_type, 0);
        assert_eq!(call(&mut p, "__set_app_type", &[1]), Ok(0));
        assert_eq!(p.crt_app_type, 1);
        assert_eq!(call(&mut p, "_set_app_type", &[2]), Ok(0));
        assert_eq!(p.crt_app_type, 2);
    }

    #[test]
    fn wide_command_line_data_import_contains_the_process_command_line() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        p.image_path = r"D:\The Sims 3\Game\Bin\TS3W.exe".into();
        p.cmdline = format!(r#""{}" -language café"#, p.image_path);

        let cell = crt::data_import(&mut p, "_wcmdln", 8).unwrap();
        let command_line = p.rdptr(cell).unwrap();
        assert_ne!(command_line, 0, "_wcmdln must point to a UTF-16 string");
        assert_eq!(p.read_wstr(command_line).unwrap(), p.cmdline);
    }

    #[test]
    fn wcsstr_searches_utf16_code_units_and_returns_the_first_guest_address() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let haystack = kernel32::alloc_block(&mut p, 128, true);
        let needle = kernel32::alloc_block(&mut p, 32, true);
        p.write_wstr(haystack, "Alpha😀BetaBeta").unwrap();
        p.write_wstr(needle, "Beta").unwrap();

        assert_eq!(
            call_x86(&mut p, "wcsstr", &[haystack, needle]),
            Ok(haystack + 14)
        );

        p.write_wstr(needle, "beta").unwrap();
        assert_eq!(call_x86(&mut p, "wcsstr", &[haystack, needle]), Ok(0));

        p.write_wstr(needle, "").unwrap();
        assert_eq!(
            call_x86(&mut p, "wcsstr", &[haystack, needle]),
            Ok(haystack)
        );
        assert!(matches!(
            call_x86(&mut p, "wcsstr", &[0, needle]),
            Err(Stop::Crash(message)) if message.contains("null haystack")
        ));
    }

    #[test]
    fn wcschr_searches_utf16_units_and_can_find_the_terminator() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let string = kernel32::alloc_block(&mut p, 128, true);
        p.write_wstr(string, "Alpha😀Beta😀").unwrap();

        assert_eq!(
            call_x86(&mut p, "wcschr", &[string, 0xd83d]),
            Ok(string + 10),
            "searches by UTF-16 code unit and returns the first high surrogate"
        );
        assert_eq!(
            call_x86(&mut p, "wcschr", &[string, 0xde00]),
            Ok(string + 12),
            "returns the matching low surrogate address"
        );
        assert_eq!(
            call_x86(&mut p, "wcschr", &[string, b'B' as u64]),
            Ok(string + 14)
        );
        assert_eq!(call_x86(&mut p, "wcschr", &[string, b'Z' as u64]), Ok(0));
        assert_eq!(call_x86(&mut p, "wcschr", &[string, 0]), Ok(string + 26));
    }

    #[test]
    fn wcspbrk_returns_the_first_string_unit_found_in_the_utf16_search_set() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let string = kernel32::alloc_block(&mut p, 128, true);
        let set = kernel32::alloc_block(&mut p, 32, true);
        p.write_wstr(string, "AA😀BC").unwrap();
        p.write_wstr(set, "zB").unwrap();

        assert_eq!(call_x86(&mut p, "wcspbrk", &[string, set]), Ok(string + 8));

        p.write_wstr(set, "xyz").unwrap();
        assert_eq!(call_x86(&mut p, "wcspbrk", &[string, set]), Ok(0));

        p.write_wstr(set, "").unwrap();
        assert_eq!(call_x86(&mut p, "wcspbrk", &[string, set]), Ok(0));
    }

    #[test]
    fn wcschr_exposes_a_null_string_as_a_guest_fault() {
        let mut p = process();
        assert!(matches!(
            call_x86(&mut p, "wcschr", &[0, b'A' as u64]),
            Err(Stop::Crash(message)) if message.contains("null")
        ));
    }

    #[test]
    fn swscanf_converts_wide_float_fields_to_float_and_double_outputs() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let input = kernel32::alloc_block(&mut p, 128, true);
        let format = kernel32::alloc_block(&mut p, 64, true);
        let output = kernel32::alloc_block(&mut p, 8, true);

        p.write_wstr(input, "2.00").unwrap();
        p.write_wstr(format, "%g").unwrap();
        assert_eq!(call_x86(&mut p, "swscanf", &[input, format, output]), Ok(1));
        assert_eq!(p.rd32(output).unwrap(), 2.0f32.to_bits());

        p.write_wstr(input, "  -3.25e1").unwrap();
        p.write_wstr(format, "%lg").unwrap();
        assert_eq!(call_x86(&mut p, "swscanf", &[input, format, output]), Ok(1));
        assert_eq!(p.rd64(output).unwrap(), (-32.5f64).to_bits());
    }

    #[test]
    fn swscanf_converts_unsigned_decimal_fields_and_matches_literal_separators() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let input = kernel32::alloc_block(&mut p, 128, true);
        let format = kernel32::alloc_block(&mut p, 64, true);
        let width = kernel32::alloc_block(&mut p, 4, true);
        let height = kernel32::alloc_block(&mut p, 4, true);

        p.write_wstr(input, "  1280, 720").unwrap();
        p.write_wstr(format, "%u,%u").unwrap();
        assert_eq!(
            call_x86(&mut p, "swscanf", &[input, format, width, height]),
            Ok(2)
        );
        assert_eq!(p.rd32(width).unwrap(), 1280);
        assert_eq!(p.rd32(height).unwrap(), 720);

        p.write_wstr(input, "-1,42").unwrap();
        assert_eq!(
            call_x86(&mut p, "swscanf", &[input, format, width, height]),
            Ok(2)
        );
        assert_eq!(p.rd32(width).unwrap(), u32::MAX);
        assert_eq!(p.rd32(height).unwrap(), 42);

        p.write_wstr(input, "1234,56").unwrap();
        p.write_wstr(format, "%3u,%u").unwrap();
        assert_eq!(
            call_x86(&mut p, "swscanf", &[input, format, width, height]),
            Ok(1),
            "the literal comma must fail when the width leaves an unmatched digit"
        );
        assert_eq!(p.rd32(width).unwrap(), 123);
        assert_eq!(p.rd32(height).unwrap(), 42);

        p.write_wstr(input, "12,x").unwrap();
        p.write_wstr(format, "%u,%u").unwrap();
        assert_eq!(
            call_x86(&mut p, "swscanf", &[input, format, width, height]),
            Ok(1),
            "a failed later conversion preserves earlier assignments"
        );
        assert_eq!(p.rd32(width).unwrap(), 12);
        assert_eq!(p.rd32(height).unwrap(), 42);
    }

    #[test]
    fn swscanf_converts_signed_and_radix_integer_fields_with_length_modifiers() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let input = kernel32::alloc_block(&mut p, 256, true);
        let format = kernel32::alloc_block(&mut p, 128, true);
        let signed = kernel32::alloc_block(&mut p, 4, true);
        let autodetected = kernel32::alloc_block(&mut p, 4, true);
        let autodetected_octal = kernel32::alloc_block(&mut p, 4, true);
        let octal = kernel32::alloc_block(&mut p, 4, true);
        let unsigned = kernel32::alloc_block(&mut p, 4, true);
        let hexadecimal = kernel32::alloc_block(&mut p, 4, true);
        let uppercase_hexadecimal = kernel32::alloc_block(&mut p, 4, true);
        let byte = kernel32::alloc_block(&mut p, 1, true);
        let long_long = kernel32::alloc_block(&mut p, 8, true);

        p.write_wstr(input, "-42 0x2a 052 052 255 ff FF -2 4294967297")
            .unwrap();
        p.write_wstr(format, "%d %i %i %o %u %x %X %hhd %lld")
            .unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "swscanf",
                &[
                    input,
                    format,
                    signed,
                    autodetected,
                    autodetected_octal,
                    octal,
                    unsigned,
                    hexadecimal,
                    uppercase_hexadecimal,
                    byte,
                    long_long,
                ],
            ),
            Ok(9)
        );
        assert_eq!(p.rd32(signed).unwrap(), (-42i32) as u32);
        assert_eq!(p.rd32(autodetected).unwrap(), 42);
        assert_eq!(p.rd32(autodetected_octal).unwrap(), 42);
        assert_eq!(p.rd32(octal).unwrap(), 42);
        assert_eq!(p.rd32(unsigned).unwrap(), 255);
        assert_eq!(p.rd32(hexadecimal).unwrap(), 255);
        assert_eq!(p.rd32(uppercase_hexadecimal).unwrap(), 255);
        assert_eq!(p.rd8(byte).unwrap(), (-2i8) as u8);
        assert_eq!(p.rd64(long_long).unwrap(), 4_294_967_297);
    }

    #[test]
    fn swscanf_honors_integer_field_width_suppression_and_i32_i64_lengths() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let input = kernel32::alloc_block(&mut p, 128, true);
        let format = kernel32::alloc_block(&mut p, 128, true);
        let remainder = kernel32::alloc_block(&mut p, 4, true);
        let short = kernel32::alloc_block(&mut p, 2, true);
        let byte = kernel32::alloc_block(&mut p, 1, true);
        let wide = kernel32::alloc_block(&mut p, 8, true);

        p.write_wstr(input, "123 655 255 10").unwrap();
        p.write_wstr(format, "%*2d%d %3hd %hhu %I64x").unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "swscanf",
                &[input, format, remainder, short, byte, wide],
            ),
            Ok(4)
        );
        assert_eq!(p.rd32(remainder).unwrap(), 3);
        assert_eq!(p.rd16(short).unwrap(), 655);
        assert_eq!(p.rd8(byte).unwrap(), 255);
        assert_eq!(p.rd64(wide).unwrap(), 16);

        p.write_wstr(input, "-2 4294967297").unwrap();
        p.write_wstr(format, "%I32d %I64u").unwrap();
        assert_eq!(
            call_x86(&mut p, "swscanf", &[input, format, remainder, wide]),
            Ok(2)
        );
        assert_eq!(p.rd32(remainder).unwrap(), (-2i32) as u32);
        assert_eq!(p.rd64(wide).unwrap(), 4_294_967_297);
    }

    #[test]
    fn swscanf_returns_scan_counts_and_exposes_unmodeled_conversions() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let input = kernel32::alloc_block(&mut p, 128, true);
        let format = kernel32::alloc_block(&mut p, 64, true);
        let output = kernel32::alloc_block(&mut p, 8, true);

        p.wr32(output, 0xdead_beef).unwrap();
        p.write_wstr(input, "words").unwrap();
        p.write_wstr(format, "%g").unwrap();
        assert_eq!(call_x86(&mut p, "swscanf", &[input, format, output]), Ok(0));
        assert_eq!(p.rd32(output).unwrap(), 0xdead_beef);

        p.write_wstr(input, "").unwrap();
        assert_eq!(
            call_x86(&mut p, "swscanf", &[input, format, output]),
            Ok(u32::MAX as u64),
            "an input failure before any assignment returns EOF"
        );

        p.write_wstr(input, "12").unwrap();
        p.write_wstr(format, "%s").unwrap();
        let unsupported = call_x86(&mut p, "swscanf", &[input, format, output]);
        assert!(
            matches!(
                unsupported.clone(),
                Err(Stop::Unimplemented { name, .. }) if name.contains("%s")
            ),
            "unexpected `%s` scan result: {unsupported:?}"
        );
    }

    #[test]
    fn wcsstr_handles_a_long_repetitive_near_match() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let haystack = kernel32::alloc_block(&mut p, 40_000, true);
        let needle = kernel32::alloc_block(&mut p, 20_000, true);
        p.write_wstr(haystack, &format!("{}b", "a".repeat(16_384)))
            .unwrap();
        p.write_wstr(needle, &format!("{}c", "a".repeat(8_192)))
            .unwrap();

        assert_eq!(call_x86(&mut p, "wcsstr", &[haystack, needle]), Ok(0));
    }

    #[test]
    fn wcsncpy_copies_utf16_units_pads_short_sources_and_keeps_truncation_unterminated() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let destination = kernel32::alloc_block(&mut p, 128, true);
        let source = kernel32::alloc_block(&mut p, 128, true);
        p.write_wstr(source, "A😀").unwrap();
        p.write_wstr(destination, "XXXXXXXX").unwrap();

        assert_eq!(
            call_x86(&mut p, "wcsncpy", &[destination, source, 6]),
            Ok(destination)
        );
        assert_eq!(p.rd16(destination).unwrap(), b'A' as u16);
        assert_eq!(p.rd16(destination + 2).unwrap(), 0xd83d);
        assert_eq!(p.rd16(destination + 4).unwrap(), 0xde00);
        assert_eq!(p.rd16(destination + 6).unwrap(), 0);
        assert_eq!(p.rd16(destination + 8).unwrap(), 0);
        assert_eq!(p.rd16(destination + 10).unwrap(), 0);

        p.write_wstr(source, "Alpha").unwrap();
        p.write_wstr(destination, "ZZZZ").unwrap();
        assert_eq!(
            call_x86(&mut p, "wcsncpy", &[destination, source, 2]),
            Ok(destination)
        );
        assert_eq!(p.rd16(destination).unwrap(), b'A' as u16);
        assert_eq!(p.rd16(destination + 2).unwrap(), b'l' as u16);
        assert_eq!(p.rd16(destination + 4).unwrap(), b'Z' as u16);
    }

    #[test]
    fn wcsncpy_does_not_fake_success_for_ucrt_invalid_parameter_calls() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let destination = kernel32::alloc_block(&mut p, 16, true);
        let source = kernel32::alloc_block(&mut p, 16, true);
        assert!(matches!(
            call_x86(&mut p, "wcsncpy", &[destination, source, 0]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("wcsncpy") && name.contains("invalid-parameter")
        ));
    }

    #[test]
    fn wcsicmp_uses_c_locale_case_mapping_and_preserves_utf16_ordering() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let first = kernel32::alloc_block(&mut p, 64, true);
        let second = kernel32::alloc_block(&mut p, 64, true);

        p.write_wstr(first, "ReadMe").unwrap();
        p.write_wstr(second, "rEaDmE").unwrap();
        assert_eq!(call_x86(&mut p, "_wcsicmp", &[first, second]), Ok(0));

        p.write_wstr(first, "Alpha").unwrap();
        p.write_wstr(second, "beta").unwrap();
        assert!((call_x86(&mut p, "wcsicmp", &[first, second]).unwrap() as i32) < 0);
        assert!((call_x86(&mut p, "wcsicmp", &[second, first]).unwrap() as i32) > 0);

        p.write_wstr(first, "Ä").unwrap();
        p.write_wstr(second, "ä").unwrap();
        assert!((call_x86(&mut p, "wcsicmp", &[first, second]).unwrap() as i32) < 0);
    }

    #[test]
    fn wcslwr_mutates_utf16_in_the_supported_c_locale_and_returns_its_input() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let string = kernel32::alloc_block(&mut p, 32, true);
        p.write_wstr(string, "SiMs Ä").unwrap();

        assert_eq!(call_x86(&mut p, "_wcslwr", &[string]), Ok(string));
        assert_eq!(p.read_wstr(string).unwrap(), "sims Ä");
        assert_eq!(call_x86(&mut p, "wcslwr", &[string]), Ok(string));
    }

    #[test]
    fn wcslwr_exposes_the_untranslated_invalid_parameter_path() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        assert!(matches!(
            call_x86(&mut p, "_wcslwr", &[0]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("_wcslwr") && name.contains("invalid-parameter")
        ));
    }

    #[test]
    fn wcsicmp_exposes_the_untranslated_invalid_parameter_path() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let valid = kernel32::alloc_block(&mut p, 16, true);
        p.write_wstr(valid, "valid").unwrap();

        assert!(matches!(
            call_x86(&mut p, "_wcsicmp", &[0, valid]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("wcsicmp") && name.contains("invalid-parameter")
        ));
    }

    #[test]
    fn wcsnicmp_compares_only_the_requested_utf16_units_and_stops_at_nul() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let first = kernel32::alloc_block(&mut p, 64, true);
        let second = kernel32::alloc_block(&mut p, 64, true);

        p.write_wstr(first, "AlphaSuffix").unwrap();
        p.write_wstr(second, "alphaDifferent").unwrap();
        assert_eq!(call_x86(&mut p, "_wcsnicmp", &[first, second, 5]), Ok(0));
        assert!(
            (call_x86(&mut p, "_wcsnicmp", &[first, second, 6]).unwrap() as i32) > 0,
            "the sixth code unit must be compared when count is six"
        );
        assert_eq!(call_x86(&mut p, "_wcsnicmp", &[first, second, 0]), Ok(0));

        p.write_wstr(first, "ab").unwrap();
        p.write_wstr(second, "ABx").unwrap();
        assert!(
            (call_x86(&mut p, "_wcsnicmp", &[first, second, 8]).unwrap() as i32) < 0,
            "a terminator reached before count makes the shorter string lesser"
        );
        assert!(
            (call_x86(&mut p, "wcsnicmp", &[first, second, 8]).unwrap() as i32) < 0,
            "the legacy alias retains the same bounded comparison semantics"
        );

        p.write_wstr(first, "Ä").unwrap();
        p.write_wstr(second, "ä").unwrap();
        assert!(
            (call_x86(&mut p, "_wcsnicmp", &[first, second, 1]).unwrap() as i32) < 0,
            "the supported default C locale only folds ASCII wide characters"
        );
    }

    #[test]
    fn wcsnicmp_exposes_the_untranslated_invalid_parameter_path() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let valid = kernel32::alloc_block(&mut p, 16, true);
        p.write_wstr(valid, "valid").unwrap();

        assert!(matches!(
            call_x86(&mut p, "_wcsnicmp", &[0, valid, 4]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("_wcsnicmp") && name.contains("invalid-parameter")
        ));

        p.write_bytes(CODE, &[0xc3]).unwrap();
        p.extra.invalid_parameter_handler = CODE;
        assert_eq!(
            call_x86(&mut p, "_wcsnicmp", &[0, valid, 4]),
            Ok(i32::MAX as u32 as u64),
            "after a returning invalid-parameter handler, the CRT returns _NLSCMPERROR"
        );
        let errno = super::errno_cell(&mut p);
        assert_eq!(p.rd32(errno).unwrap(), 22, "EINVAL is set after validation");
    }

    #[test]
    fn wcsnicmp_exposes_counts_beyond_its_translated_read_bound() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let first = kernel32::alloc_block(&mut p, 16, true);
        let second = kernel32::alloc_block(&mut p, 16, true);
        p.write_wstr(first, "a").unwrap();
        p.write_wstr(second, "A").unwrap();

        let unsupported = call_x86(
            &mut p,
            "_wcsnicmp",
            &[first, second, super::CRT_WIDE_STRING_MAX_UNITS + 1],
        );
        assert!(matches!(
            unsupported,
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("_wcsnicmp count")
        ));
    }

    #[test]
    fn wgetmainargs_returns_pe32_wide_argv_and_environment_arrays() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        p.image_path = r"D:\The Sims 3\Game\Bin\TS3W.exe".into();
        p.cmdline = format!(r#""{}" -language café"#, p.image_path);
        let argc_out = kernel32::alloc_block(&mut p, 4, true);
        let argv_out = kernel32::alloc_block(&mut p, 8, true);
        let env_out = kernel32::alloc_block(&mut p, 8, true);
        p.wr32(argv_out + 4, 0xfeed_face).unwrap();
        p.wr32(env_out + 4, 0xcafe_babe).unwrap();

        assert_eq!(
            call_x86(
                &mut p,
                "__wgetmainargs",
                &[argc_out, argv_out, env_out, 0, 0]
            ),
            Ok(0)
        );

        assert_eq!(p.rd32(argc_out).unwrap(), 3);
        assert_eq!(p.rd32(argv_out + 4).unwrap(), 0xfeed_face);
        assert_eq!(p.rd32(env_out + 4).unwrap(), 0xcafe_babe);
        let argv = p.rdptr(argv_out).unwrap();
        let argv0 = p.rdptr(argv).unwrap();
        let argv1 = p.rdptr(argv + 4).unwrap();
        let argv2 = p.rdptr(argv + 8).unwrap();
        assert_eq!(p.read_wstr(argv0).unwrap(), p.image_path);
        assert_eq!(p.read_wstr(argv1).unwrap(), "-language");
        assert_eq!(p.read_wstr(argv2).unwrap(), "café");
        assert_eq!(p.rdptr(argv + 12).unwrap(), 0);

        let envp = p.rdptr(env_out).unwrap();
        assert_ne!(envp, 0);
        let expected_environment = [
            "SystemRoot=C:\\Windows",
            "TEMP=C:\\Temp",
            "PATH=C:\\Windows\\System32",
            "USERPROFILE=C:\\Users\\AnyWASM",
            "HOMEDRIVE=C:",
            "HOMEPATH=\\Users\\AnyWASM",
            "USERNAME=AnyWASM",
            "COMPUTERNAME=ANYWASM",
            "OS=Windows_NT",
        ];
        for (index, expected) in expected_environment.iter().enumerate() {
            let entry = p.rdptr(envp + index as u64 * 4).unwrap();
            assert_ne!(entry, 0, "environment entry {index} should be present");
            assert_eq!(p.read_wstr(entry).unwrap(), *expected);
        }
        assert_eq!(
            p.rdptr(envp + expected_environment.len() as u64 * 4)
                .unwrap(),
            0
        );
    }

    #[test]
    fn getmainargs_returns_pe32_narrow_argv_with_four_byte_slots() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        p.image_path = r"C:\Game\TS3W.exe".into();
        p.cmdline = format!(r#""{}" -quality high"#, p.image_path);
        let argc_out = kernel32::alloc_block(&mut p, 4, true);
        let argv_out = kernel32::alloc_block(&mut p, 4, true);
        let env_out = kernel32::alloc_block(&mut p, 4, true);

        assert_eq!(
            call_x86(
                &mut p,
                "__getmainargs",
                &[argc_out, argv_out, env_out, 0, 0]
            ),
            Ok(0)
        );

        assert_eq!(p.rd32(argc_out).unwrap(), 3);
        let argv = p.rdptr(argv_out).unwrap();
        assert_eq!(p.read_cstr(p.rdptr(argv).unwrap()).unwrap(), p.image_path);
        assert_eq!(p.read_cstr(p.rdptr(argv + 4).unwrap()).unwrap(), "-quality");
        assert_eq!(p.read_cstr(p.rdptr(argv + 8).unwrap()).unwrap(), "high");
        assert_eq!(p.rdptr(argv + 12).unwrap(), 0);
        assert_ne!(p.rdptr(env_out).unwrap(), 0);
    }

    #[test]
    fn initial_environment_pointer_stays_separate_from_mutable_environment() {
        let mut p = process();
        let initial_cell = crt::data_import(&mut p, "__initenv", 8).unwrap();
        let initial_env = p.rdptr(initial_cell).unwrap();
        let initial_path = p.read_cstr(p.rdptr(initial_env + 2 * 8).unwrap()).unwrap();
        let replacement = c_string(&mut p, "PATH=C:\\Game\\Bin");

        assert_eq!(call(&mut p, "_putenv", &[replacement]), Ok(0));

        assert_eq!(p.rdptr(initial_cell).unwrap(), initial_env);
        assert_eq!(
            p.read_cstr(p.rdptr(initial_env + 2 * 8).unwrap()).unwrap(),
            initial_path
        );
        let current_cell = crt::data_import(&mut p, "_environ", 8).unwrap();
        let current_env = p.rdptr(current_cell).unwrap();
        assert_ne!(current_env, initial_env);
        assert_eq!(
            p.read_cstr(p.rdptr(current_env + 2 * 8).unwrap()).unwrap(),
            "PATH=C:\\Game\\Bin"
        );
    }
}

#[cfg(test)]
mod localtime64_tests {
    use super::lookup;
    use crate::{process::Process, vfs::Vfs};
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RWX};

    const DATA: u64 = 0x80000;
    const TIME_VALUE: u64 = DATA + 0x100;
    const HANDLER: u64 = DATA + 0x500;
    const STACK: u64 = DATA + 0x800;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(DATA, PAGE_SIZE, PROT_RWX));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p
    }

    fn call(p: &mut Process, value: i64) -> u64 {
        p.wr64(TIME_VALUE, value as u64).unwrap();
        p.wr32(STACK + 4, TIME_VALUE as u32).unwrap();
        lookup("_localtime64").expect("_localtime64 must have a real CRT translation")(p).unwrap()
    }

    fn tm(p: &Process, address: u64) -> [i32; 9] {
        std::array::from_fn(|index| p.rd32(address + index as u64 * 4).unwrap() as i32)
    }

    fn allow_invalid_parameter_recovery(p: &mut Process) {
        p.write_bytes(HANDLER, &[0xc3]).unwrap();
        p.extra.invalid_parameter_handler = HANDLER;
    }

    #[test]
    fn localtime64_returns_utc_tm_fields_for_epoch_and_leap_day() {
        let mut p = process();
        let epoch = call(&mut p, 0);
        assert_ne!(epoch, 0);
        assert_eq!(tm(&p, epoch), [0, 0, 0, 1, 0, 70, 4, 0, 0]);

        let leap_day = call(&mut p, 951_782_400); // 2000-02-29 00:00:00 UTC
        assert_eq!(leap_day, epoch);
        assert_eq!(tm(&p, leap_day), [0, 0, 0, 29, 1, 100, 2, 59, 0]);
    }

    #[test]
    fn localtime64_has_one_result_buffer_per_guest_thread() {
        let mut p = process();
        let main_buffer = call(&mut p, 1_767_225_600);
        p.current_thread_id += 1;
        let worker_buffer = call(&mut p, 0);

        assert_ne!(worker_buffer, main_buffer);
        assert_eq!(tm(&p, main_buffer), [0, 0, 0, 1, 0, 126, 4, 0, 0]);
        assert_eq!(tm(&p, worker_buffer), [0, 0, 0, 1, 0, 70, 4, 0, 0]);
    }

    #[test]
    fn localtime64_rejects_out_of_range_times_and_null_input() {
        let mut p = process();
        allow_invalid_parameter_recovery(&mut p);
        assert_eq!(call(&mut p, -1), 0);
        assert_eq!(p.rd32(p.extra.errno_cell).unwrap(), 22); // EINVAL
        p.wr32(STACK + 4, 0).unwrap();
        assert_eq!(
            lookup("_localtime64").unwrap()(&mut p).unwrap(),
            0,
            "a null time pointer must fail explicitly"
        );
        assert_eq!(p.rd32(p.extra.errno_cell).unwrap(), 22);
    }
}

#[cfg(test)]
mod msvc_streambuf_tests {
    use super::{msvc_basic_streambuf_char_uflow, msvc_basic_streambuf_char_xsgetn};
    use super::{msvc_basic_streambuf_wchar_uflow, msvc_basic_streambuf_wchar_xsgetn};
    use crate::process::Process;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, R8, RCX, RDX, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW, PROT_RWX};

    const OBJECT: u64 = 0x40000;
    const VTABLE: u64 = OBJECT + 0x100;
    const INPUT: u64 = OBJECT + 0x200;
    const OUTPUT: u64 = OBJECT + 0x300;
    const STACK: u64 = 0x80000;
    const CODE: u64 = 0x50000;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(OBJECT, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(CODE, PAGE_SIZE, PROT_RWX));
        p.cpu.mode = CpuMode::X86_64;
        p.cpu.gpr[RSP] = STACK + 0x800;
        p.wr64(OBJECT, VTABLE).unwrap();
        p.wr64(OBJECT + 0x38, OBJECT + 0x28).unwrap();
        p.wr64(OBJECT + 0x50, OBJECT + 0x48).unwrap();
        p
    }

    fn set_args(p: &mut Process, this: u64, buffer: u64, count: u64) {
        p.cpu.gpr[RCX] = this;
        p.cpu.gpr[RDX] = buffer;
        p.cpu.gpr[R8] = count;
    }

    fn install_base_input_virtuals(p: &mut Process, wide: bool) {
        let (underflow_name, uflow_name) = if wide {
            (
                "__anywasm_basic_streambuf_wchar_underflow",
                "__anywasm_basic_streambuf_wchar_uflow",
            )
        } else {
            (
                "__anywasm_basic_streambuf_char_underflow",
                "__anywasm_basic_streambuf_char_uflow",
            )
        };
        let underflow = p.thunk_for("msvcp", underflow_name, false).unwrap();
        let uflow = p.thunk_for("msvcp", uflow_name, false).unwrap();
        p.wr64(VTABLE + 6 * 8, underflow).unwrap();
        p.wr64(VTABLE + 7 * 8, uflow).unwrap();
    }

    #[test]
    fn narrow_uflow_and_xsgetn_advance_buffer_and_stop_at_eof() {
        let mut p = process();
        install_base_input_virtuals(&mut p, false);
        p.write_bytes(INPUT, b"abc").unwrap();
        p.wr64(OBJECT + 0x28, INPUT).unwrap();
        p.wr32(OBJECT + 0x48, 3).unwrap();

        set_args(&mut p, OBJECT, OUTPUT, 0);
        assert_eq!(msvc_basic_streambuf_char_uflow(&mut p), Ok(b'a' as u64));
        assert_eq!(p.rd64(OBJECT + 0x28).unwrap(), INPUT + 1);
        assert_eq!(p.rd32(OBJECT + 0x48).unwrap(), 2);

        set_args(&mut p, OBJECT, OUTPUT, 4);
        assert_eq!(msvc_basic_streambuf_char_xsgetn(&mut p), Ok(2));
        assert_eq!(p.read_bytes(OUTPUT, 2).unwrap(), b"bc");
        assert_eq!(p.rd64(OBJECT + 0x28).unwrap(), INPUT + 3);
        assert_eq!(p.rd32(OBJECT + 0x48).unwrap(), 0);

        set_args(&mut p, OBJECT, OUTPUT, 1);
        assert_eq!(msvc_basic_streambuf_char_uflow(&mut p), Ok(u32::MAX as u64));
        set_args(&mut p, OBJECT, OUTPUT, u64::MAX);
        assert_eq!(msvc_basic_streambuf_char_xsgetn(&mut p), Ok(0));
    }

    #[test]
    fn wide_uflow_and_xsgetn_copy_utf16_code_units() {
        let mut p = process();
        install_base_input_virtuals(&mut p, true);
        p.wr16(INPUT, 0x0041).unwrap();
        p.wr16(INPUT + 2, 0x20ac).unwrap();
        p.wr16(INPUT + 4, 0x03a9).unwrap();
        p.wr64(OBJECT + 0x28, INPUT).unwrap();
        p.wr32(OBJECT + 0x48, 3).unwrap();

        set_args(&mut p, OBJECT, OUTPUT, 0);
        assert_eq!(msvc_basic_streambuf_wchar_uflow(&mut p), Ok(0x0041));
        set_args(&mut p, OBJECT, OUTPUT, 3);
        assert_eq!(msvc_basic_streambuf_wchar_xsgetn(&mut p), Ok(2));
        assert_eq!(p.rd16(OUTPUT).unwrap(), 0x20ac);
        assert_eq!(p.rd16(OUTPUT + 2).unwrap(), 0x03a9);
        assert_eq!(p.rd16(OBJECT + 0x48).unwrap(), 0);
        set_args(&mut p, OBJECT, OUTPUT, 1);
        assert_eq!(
            msvc_basic_streambuf_wchar_uflow(&mut p),
            Ok(u16::MAX as u64)
        );
    }

    #[test]
    fn uflow_calls_virtual_underflow_and_consumes_its_refilled_area() {
        let mut p = process();
        const UNDERFLOW: u64 = CODE + 0x100;
        let mut code = vec![0x48, 0xb8]; // mov rax, address of _Gnext
        code.extend_from_slice(&(OBJECT + 0x28).to_le_bytes());
        code.extend_from_slice(&[0x48, 0xba]); // mov rdx, input character address
        code.extend_from_slice(&INPUT.to_le_bytes());
        code.extend_from_slice(&[0x48, 0x89, 0x10]); // mov [rax], rdx
        code.extend_from_slice(&[0x48, 0xb8]); // mov rax, address of _Gcount
        code.extend_from_slice(&(OBJECT + 0x48).to_le_bytes());
        code.extend_from_slice(&[0xc7, 0x00, 1, 0, 0, 0]); // mov dword ptr [rax], 1
        code.extend_from_slice(&[0xb8, b'z', 0, 0, 0, 0xc3]); // mov eax, 'z'; ret
        p.write_bytes(UNDERFLOW, &code).unwrap();
        p.wr64(VTABLE + 6 * 8, UNDERFLOW).unwrap();
        p.wr64(OBJECT + 0x28, 0).unwrap();
        p.wr32(OBJECT + 0x48, 0).unwrap();
        p.wr8(INPUT, b'z').unwrap();

        set_args(&mut p, OBJECT, OUTPUT, 0);
        assert_eq!(msvc_basic_streambuf_char_uflow(&mut p), Ok(b'z' as u64));
        assert_eq!(p.rd64(OBJECT + 0x28).unwrap(), INPUT + 1);
        assert_eq!(p.rd32(OBJECT + 0x48).unwrap(), 0);
    }
}

#[cfg(test)]
mod missing_msvcrt_string_tests {
    use super::lookup;
    use crate::{
        hle::kernel32,
        process::{Process, Stop},
        vfs::Vfs,
    };
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const STACK: u64 = 0x80000;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK + 0x800;
        p
    }

    fn call(p: &mut Process, name: &str, args: &[u64]) -> Result<u64, Stop> {
        let sp = STACK + 0x800;
        p.cpu.gpr[RSP] = sp;
        p.wr32(sp, 0)?;
        for (index, value) in args.iter().enumerate() {
            p.wr32(sp + 4 + index as u64 * 4, *value as u32)?;
        }
        lookup(name).expect("CRT import should have a translation")(p)
    }

    fn call_double_ints(
        p: &mut Process,
        name: &str,
        value: f64,
        digits: i32,
        decimal_pointer: u64,
        sign_pointer: u64,
    ) -> Result<u64, Stop> {
        let stack = STACK + 0x800;
        p.cpu.gpr[RSP] = stack;
        p.wr32(stack, 0)?;
        p.wr64(stack + 4, value.to_bits())?;
        p.wr32(stack + 12, digits as u32)?;
        p.wr32(stack + 16, decimal_pointer as u32)?;
        p.wr32(stack + 20, sign_pointer as u32)?;
        lookup(name).expect("CRT double conversion should have a translation")(p)
    }

    fn wide(p: &mut Process, value: &str) -> u64 {
        let units = value.encode_utf16().count() as u64;
        let address = kernel32::alloc_block(p, (units + 1) * 2, true);
        p.write_wstr(address, value).unwrap();
        address
    }

    #[test]
    fn wcsncmp_compares_unsigned_utf16_units_and_stops_at_nul() {
        let mut p = process();
        let left = wide(&mut p, "AΩ");
        let same_prefix = wide(&mut p, "AΩZ");
        let lower = wide(&mut p, "Aβ");
        assert_eq!(call(&mut p, "wcsncmp", &[left, same_prefix, 2]), Ok(0));
        assert!((call(&mut p, "wcsncmp", &[left, lower, 3]).unwrap() as i32) < 0);
        assert_eq!(call(&mut p, "wcsncmp", &[left, lower, 0]), Ok(0));
    }

    #[test]
    fn wcsrchr_finds_last_unit_and_can_find_the_terminator() {
        let mut p = process();
        let text = wide(&mut p, "alpha beta alpha");
        assert_eq!(call(&mut p, "wcsrchr", &[text, b'a' as u64]), Ok(text + 30));
        assert_eq!(call(&mut p, "wcsrchr", &[text, 0]), Ok(text + 32));
        assert_eq!(call(&mut p, "wcsrchr", &[text, b'z' as u64]), Ok(0));
    }

    #[test]
    fn secure_wide_copy_and_concat_check_capacity_and_write_terminators() {
        let mut p = process();
        let dest = kernel32::alloc_block(&mut p, 32, true);
        let source = wide(&mut p, "two");
        assert_eq!(call(&mut p, "wcscpy_s", &[dest, 8, source]), Ok(0));
        assert_eq!(p.read_wstr(dest).unwrap(), "two");
        let suffix = wide(&mut p, " words");
        assert!(matches!(
            call(&mut p, "wcscat_s", &[dest, 8, suffix]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("wcscat_s") && name.contains("invalid-parameter")
        ));
        assert_eq!(
            p.read_wstr(dest).unwrap(),
            "",
            "secure failure clears destination"
        );
        assert_eq!(call(&mut p, "wcscpy_s", &[dest, 8, source]), Ok(0));
        assert_eq!(call(&mut p, "wcscat_s", &[dest, 16, suffix]), Ok(0));
        assert_eq!(p.read_wstr(dest).unwrap(), "two words");
    }

    #[test]
    fn wide_integer_conversions_report_end_pointer_and_signedness() {
        let mut p = process();
        let text = wide(&mut p, "  -0x2a-tail");
        let end = kernel32::alloc_block(&mut p, 4, true);
        assert_eq!(
            call(&mut p, "wcstol", &[text, end, 0]),
            Ok((-42i32) as u32 as u64)
        );
        assert_eq!(p.rdptr(end).unwrap(), text + 14);

        let unsigned = wide(&mut p, "-1!");
        assert_eq!(
            call(&mut p, "wcstoul", &[unsigned, end, 10]),
            Ok(u32::MAX as u64)
        );
        assert_eq!(p.rdptr(end).unwrap(), unsigned + 4);
    }

    #[test]
    fn wide_float_and_multibyte_conversions_use_the_c_windows_1252_page() {
        let mut p = process();
        let text = wide(&mut p, "-12.5tail");
        let end = kernel32::alloc_block(&mut p, 4, true);
        assert_eq!(call(&mut p, "wcstod", &[text, end]), Ok(0));
        assert_eq!(p.cpu.fpu_st[p.cpu.fpu_top as usize], -12.5);
        assert_eq!(p.rdptr(end).unwrap(), text + 10);

        let wide_source = wide(&mut p, "A€");
        let narrow = kernel32::alloc_block(&mut p, 8, true);
        assert_eq!(call(&mut p, "wcstombs", &[narrow, wide_source, 4]), Ok(2));
        assert_eq!(p.read_bytes(narrow, 3).unwrap(), [b'A', 0x80, 0]);

        let unrepresentable = wide(&mut p, "AΩ");
        assert_eq!(
            call(&mut p, "wcstombs", &[narrow, unrepresentable, 4]),
            Ok(u64::MAX)
        );
        assert_eq!(p.read_bytes(narrow, 1).unwrap(), [b'A']);
        let errno = super::errno_cell(&mut p);
        assert_eq!(p.rd32(errno).unwrap(), 42); // EILSEQ

        let narrow_source = kernel32::alloc_block(&mut p, 4, true);
        p.write_bytes(narrow_source, &[b'A', 0x80, 0]).unwrap();
        let wide_dest = kernel32::alloc_block(&mut p, 8, true);
        assert_eq!(
            call(&mut p, "mbstowcs", &[wide_dest, narrow_source, 4]),
            Ok(2)
        );
        assert_eq!(p.read_wstr(wide_dest).unwrap(), "A€");
    }

    #[test]
    fn secure_narrow_printf_uses_x86_variadic_stack_and_truncate_contract() {
        let mut p = process();
        let buffer = kernel32::alloc_block(&mut p, 16, true);
        let format = kernel32::alloc_block(&mut p, 16, true);
        p.write_cstr(format, "%s:%d").unwrap();
        let text = kernel32::alloc_block(&mut p, 4, true);
        p.write_cstr(text, "x").unwrap();
        assert_eq!(
            call(&mut p, "_snprintf_s", &[buffer, 8, 7, format, text, 42]),
            Ok(4)
        );
        assert_eq!(p.read_cstr(buffer).unwrap(), "x:42");

        let trunc = kernel32::alloc_block(&mut p, 4, true);
        assert_eq!(
            call(
                &mut p,
                "_snprintf_s",
                &[trunc, 4, u32::MAX as u64, format, text, 42]
            ),
            Ok(u32::MAX as u64)
        );
        assert_eq!(p.read_cstr(trunc).unwrap(), "x:4");
    }

    #[test]
    fn secure_va_list_printf_uses_four_byte_x86_slots() {
        let mut p = process();
        let buffer = kernel32::alloc_block(&mut p, 16, true);
        let format = kernel32::alloc_block(&mut p, 16, true);
        p.write_cstr(format, "%s:%d").unwrap();
        let text = kernel32::alloc_block(&mut p, 4, true);
        p.write_cstr(text, "x").unwrap();
        let args = kernel32::alloc_block(&mut p, 8, true);
        p.wr32(args, text as u32).unwrap();
        p.wr32(args + 4, 42).unwrap();
        assert_eq!(
            call(&mut p, "_vsnprintf_s", &[buffer, 8, 7, format, args]),
            Ok(4)
        );
        assert_eq!(p.read_cstr(buffer).unwrap(), "x:42");
    }

    #[test]
    fn wide_secure_va_printf_writes_utf16_and_counts_wide_units() {
        let mut p = process();
        let buffer = kernel32::alloc_block(&mut p, 16, true);
        let format = wide(&mut p, "%d");
        let args = kernel32::alloc_block(&mut p, 4, true);
        p.wr32(args, 12).unwrap();
        assert_eq!(
            call(&mut p, "_vswprintf_c_l", &[buffer, 8, format, 0, args]),
            Ok(2)
        );
        assert_eq!(p.read_wstr(buffer).unwrap(), "12");
    }

    #[test]
    fn time_apis_break_down_normalize_and_format_the_utc_calendar() {
        let mut p = process();
        let time_pointer = kernel32::alloc_block(&mut p, 8, true);
        p.wr64(time_pointer, 0).unwrap();
        let tm = call(&mut p, "_gmtime64", &[time_pointer]).unwrap();
        assert_ne!(tm, 0);
        assert_eq!(p.rd32(tm).unwrap() as i32, 0);
        assert_eq!(p.rd32(tm + 4).unwrap() as i32, 0);
        assert_eq!(p.rd32(tm + 8).unwrap() as i32, 0);
        assert_eq!(p.rd32(tm + 12).unwrap() as i32, 1);
        assert_eq!(p.rd32(tm + 16).unwrap() as i32, 0);
        assert_eq!(p.rd32(tm + 20).unwrap() as i32, 70);
        assert_eq!(p.rd32(tm + 24).unwrap() as i32, 4);

        let output = kernel32::alloc_block(&mut p, 64, true);
        let format = kernel32::alloc_block(&mut p, 64, true);
        p.write_cstr(format, "%Y-%m-%d %H:%M:%S %a %Z").unwrap();
        assert_eq!(call(&mut p, "strftime", &[output, 64, format, tm]), Ok(27));
        assert_eq!(p.read_cstr(output).unwrap(), "1970-01-01 00:00:00 Thu UTC");
        let asctime = call(&mut p, "asctime", &[tm]).unwrap();
        assert_eq!(p.read_cstr(asctime).unwrap(), "Thu Jan  1 00:00:00 1970\n");
    }

    #[test]
    fn mktime64_normalizes_leap_day_and_x86_difftime_reads_two_qwords() {
        let mut p = process();
        let tm = kernel32::alloc_block(&mut p, 36, true);
        for (index, field) in [56i32, 34, 12, 29, 1, 120, 0, 0, -1]
            .into_iter()
            .enumerate()
        {
            p.wr32(tm + index as u64 * 4, field as u32).unwrap();
        }
        assert_eq!(call(&mut p, "_mktime64", &[tm]), Ok(1_582_979_696));
        assert_eq!(p.rd32(tm + 24).unwrap() as i32, 6); // Saturday
        assert_eq!(p.rd32(tm + 28).unwrap() as i32, 59); // zero-based day of year
        assert_eq!(p.rd32(tm + 32).unwrap() as i32, 0); // UTC has no DST

        assert_eq!(
            call(&mut p, "_difftime64", &[u32::MAX as u64, 1, 1, 0]),
            Ok(0)
        );
        assert_eq!(
            p.cpu.fpu_st[p.cpu.fpu_top as usize],
            (8_589_934_591i64) as f64 - 1.0
        );
    }

    #[test]
    fn thread_locale_mode_reports_and_tracks_mode_per_guest_thread() {
        let mut p = process();
        assert_eq!(call(&mut p, "_configthreadlocale", &[-1i64 as u64]), Ok(0));
        assert_eq!(call(&mut p, "_configthreadlocale", &[1]), Ok(0));
        assert_eq!(call(&mut p, "_configthreadlocale", &[-1i64 as u64]), Ok(1));
        p.current_thread_id += 1;
        assert_eq!(call(&mut p, "_configthreadlocale", &[-1i64 as u64]), Ok(0));
        assert_eq!(call(&mut p, "_configthreadlocale", &[0]), Ok(0));
        assert_eq!(call(&mut p, "_configthreadlocale", &[-1i64 as u64]), Ok(0));

        let errno = super::errno_cell(&mut p);
        assert_eq!(
            call(&mut p, "_configthreadlocale", &[2]),
            Ok(u32::MAX as u64)
        );
        assert_eq!(p.rd32(errno).unwrap(), 22); // EINVAL
    }

    #[test]
    fn setmbcp_accepts_only_the_active_windows_1252_page() {
        let mut p = process();
        assert_eq!(call(&mut p, "_setmbcp", &[0]), Ok(0));
        assert_eq!(call(&mut p, "_setmbcp", &[1252]), Ok(0));
        assert_eq!(
            call(&mut p, "_setmbcp", &[437]),
            Err(Stop::Unimplemented {
                dll: "crt".into(),
                name: "_setmbcp code page 437 (only the active Windows-1252 page is modeled)"
                    .into()
            })
        );
    }

    #[test]
    fn mktemp_replaces_trailing_x_with_a_unique_nonexistent_vfs_name() {
        let mut p = process();
        p.current_directory = r"C:\work".into();
        let template = kernel32::alloc_block(&mut p, 64, true);
        p.write_cstr(template, "cache-XXX").unwrap();
        assert_eq!(call(&mut p, "_mktemp", &[template]), Ok(template));
        let first = p.read_cstr(template).unwrap();
        assert!(first.starts_with("cache-"));
        assert_eq!(first.len(), "cache-XXX".len());
        assert!(!p
            .vfs
            .exists(&crate::vfs::absolute(&first, &p.current_directory)));

        p.vfs
            .insert(&crate::vfs::absolute(&first, &p.current_directory), vec![]);
        p.write_cstr(template, "cache-XXX").unwrap();
        assert_eq!(call(&mut p, "_mktemp", &[template]), Ok(template));
        assert_ne!(p.read_cstr(template).unwrap(), first);

        p.write_cstr(template, "no-template").unwrap();
        let errno = super::errno_cell(&mut p);
        assert_eq!(call(&mut p, "_mktemp", &[template]), Ok(0));
        assert_eq!(p.rd32(errno).unwrap(), 22); // EINVAL
    }

    #[test]
    fn narrow_stat_and_wide_unlink_use_real_vfs_paths() {
        let mut p = process();
        p.current_directory = r"D:\The Sims 3\Game\Bin".into();
        let file = r"D:\The Sims 3\Game\Bin\cache.dat";
        p.vfs.insert(file, vec![1, 2, 3]);
        p.vfs.set_file_metadata(
            file,
            crate::vfs::FileMetadata {
                accessed: Some(100),
                modified: Some(200),
                created: Some(50),
                mode: Some(0o100600),
                link_count: Some(1),
            },
        );
        let narrow = kernel32::alloc_block(&mut p, 128, true);
        let output = kernel32::alloc_block(&mut p, 48, true);
        p.write_cstr(narrow, file).unwrap();
        let wide_path = wide(&mut p, file);
        let times = kernel32::alloc_block(&mut p, 16, true);
        p.wr64(times, 1_100).unwrap();
        p.wr64(times + 8, 2_200).unwrap();
        assert_eq!(call(&mut p, "_wutime64", &[wide_path, times]), Ok(0));
        let status = p.vfs.file_status(file).unwrap();
        assert_eq!(status.accessed, Some(1_100));
        assert_eq!(status.modified, Some(2_200));
        assert_eq!(call(&mut p, "_stat64i32", &[narrow, output]), Ok(0));
        assert_eq!(p.rd32(output + 20).unwrap(), 3);

        assert_eq!(call(&mut p, "_wunlink", &[wide_path]), Ok(0));
        assert!(!p.vfs.exists(file));
        assert_eq!(call(&mut p, "_wunlink", &[wide_path]), Ok(u32::MAX as u64));
        let errno = super::errno_cell(&mut p);
        assert_eq!(p.rd32(errno).unwrap(), 2); // ENOENT
    }

    #[test]
    fn x86_ci_unary_math_transforms_st0_and_rejects_non_x86_abi() {
        let mut p = process();
        p.cpu.fpu_top = 6;
        p.cpu.fpu_st[6] = std::f64::consts::FRAC_PI_2;
        assert_eq!(call(&mut p, "_CIsin", &[]), Ok(0));
        assert!((p.cpu.fpu_st[6] - 1.0).abs() < 1e-12);
        assert_eq!(p.cpu.fpu_top, 6, "unary x87 helper retains the ST0 slot");

        p.cpu.mode = CpuMode::X86_64;
        assert!(matches!(
            call(&mut p, "_CIcos", &[]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("outside PE32")
        ));
    }

    #[test]
    fn binary_crt_descriptors_read_seek_stat_change_mode_and_close() {
        let mut p = process();
        let path = r"C:\sample.bin";
        p.vfs.insert(path, b"abc".to_vec());
        p.vfs.set_file_metadata(
            path,
            crate::vfs::FileMetadata {
                accessed: Some(1),
                modified: Some(2),
                created: Some(3),
                mode: Some(0o100600),
                link_count: Some(1),
            },
        );
        let name = kernel32::alloc_block(&mut p, 64, true);
        let buffer = kernel32::alloc_block(&mut p, 16, true);
        let output = kernel32::alloc_block(&mut p, 48, true);
        p.write_cstr(name, path).unwrap();
        let fd = call(&mut p, "_open", &[name, 0x8000, 0]).unwrap();
        assert_eq!(fd, 3);
        assert_eq!(call(&mut p, "_setmode", &[fd, 0x8000]), Ok(0x8000));
        assert_eq!(call(&mut p, "_lseek", &[fd, 1, 0]), Ok(1));
        assert_eq!(call(&mut p, "_read", &[fd, buffer, 2]), Ok(2));
        assert_eq!(p.read_bytes(buffer, 2).unwrap(), b"bc");
        assert_eq!(call(&mut p, "_lseek", &[fd, 0, 2]), Ok(3));
        assert_eq!(call(&mut p, "_read", &[fd, buffer, 2]), Ok(0));
        assert_eq!(call(&mut p, "_fstat64i32", &[fd, output]), Ok(0));
        assert_eq!(p.rd32(output + 20).unwrap(), 3);

        assert!(matches!(
            call(&mut p, "_get_osfhandle", &[fd]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("without a translated Win32 file handle")
        ));
        assert_eq!(call(&mut p, "_close", &[fd]), Ok(0));
        let errno = super::errno_cell(&mut p);
        assert_eq!(call(&mut p, "_read", &[fd, buffer, 1]), Ok(u32::MAX as u64));
        assert_eq!(p.rd32(errno).unwrap(), 9); // EBADF
    }

    #[test]
    fn binary_crt_descriptor_writes_to_vfs_and_text_open_stops_before_mutation() {
        let mut p = process();
        let path = r"C:\write.bin";
        p.vfs.insert(path, b"old".to_vec());
        let name = kernel32::alloc_block(&mut p, 64, true);
        let buffer = kernel32::alloc_block(&mut p, 16, true);
        p.write_cstr(name, path).unwrap();
        p.write_cstr(buffer, "new!").unwrap();
        let fd = call(&mut p, "_open", &[name, 0x8002, 0]).unwrap();
        assert_eq!(call(&mut p, "_write", &[fd, buffer, 4]), Ok(4));
        assert_eq!(p.vfs.get(path).unwrap(), b"new!");
        assert_eq!(call(&mut p, "_close", &[fd]), Ok(0));

        p.write_cstr(name, r"C:\text.txt").unwrap();
        assert!(matches!(
            call(&mut p, "_open", &[name, 0x0100 | 1, 0]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("text mode")
        ));
        assert!(
            !p.vfs.exists(r"C:\text.txt"),
            "unsupported text open is preflighted"
        );
    }

    #[test]
    fn ecvt_fcvt_read_x86_stack_doubles_and_fill_outputs() {
        let mut p = process();
        let decimal = kernel32::alloc_block(&mut p, 4, true);
        let sign = kernel32::alloc_block(&mut p, 4, true);
        let ecvt = call_double_ints(&mut p, "_ecvt", -123.456, 5, decimal, sign).unwrap();
        assert_eq!(p.read_cstr(ecvt).unwrap(), "12346");
        assert_eq!(p.rd32(decimal).unwrap() as i32, 3);
        assert_eq!(p.rd32(sign).unwrap(), 1);

        let fcvt = call_double_ints(&mut p, "_fcvt", 12.3456, 2, decimal, sign).unwrap();
        assert_eq!(p.read_cstr(fcvt).unwrap(), "1235");
        assert_eq!(p.rd32(decimal).unwrap() as i32, 2);
        assert_eq!(p.rd32(sign).unwrap(), 0);
    }

    #[test]
    fn wide_searchenv_checks_current_directory_then_environment_directories() {
        let mut p = process();
        p.current_directory = r"D:\Game\Bin".into();
        p.env
            .push(("S3_PLUGIN_PATH".into(), r"D:\Shared;D:\Mods".into()));
        p.vfs.insert(r"D:\Mods\feature.package", vec![1]);
        let file = wide(&mut p, "feature.package");
        let variable = wide(&mut p, "S3_PLUGIN_PATH");
        let result = kernel32::alloc_block(&mut p, 512, true);

        assert_eq!(
            call(&mut p, "_wsearchenv", &[file, variable, result]),
            Ok(0)
        );
        assert_eq!(
            p.read_wstr(result).unwrap(),
            ["D:", "Mods", "feature.package"].join("\\")
        );

        p.vfs.insert(r"D:\Game\Bin\feature.package", vec![2]);
        assert_eq!(
            call(&mut p, "_wsearchenv", &[file, variable, result]),
            Ok(0)
        );
        assert_eq!(p.read_wstr(result).unwrap(), r"D:\Game\Bin\feature.package");

        p.write_wstr(file, "missing.package").unwrap();
        assert_eq!(
            call(&mut p, "_wsearchenv", &[file, variable, result]),
            Ok(0)
        );
        assert_eq!(p.read_wstr(result).unwrap(), "");
    }

    #[test]
    fn wide_rename_moves_local_bytes_and_metadata_without_overwriting() {
        let mut p = process();
        let source_path = r"C:\Cache\source.package";
        let destination_path = r"C:\Cache\renamed.package";
        p.vfs.insert(source_path, vec![1, 2, 3]);
        p.vfs.set_file_metadata(
            source_path,
            crate::vfs::FileMetadata {
                accessed: Some(10),
                modified: Some(20),
                created: Some(30),
                mode: Some(0o100600),
                link_count: Some(1),
            },
        );
        p.vfs.set_file_attributes(source_path, 0x20); // FILE_ATTRIBUTE_ARCHIVE
        let source = wide(&mut p, source_path);
        let destination = wide(&mut p, destination_path);

        assert_eq!(call(&mut p, "_wrename", &[source, destination]), Ok(0));
        assert!(!p.vfs.exists(source_path));
        assert_eq!(p.vfs.get(destination_path).unwrap(), &[1, 2, 3]);
        assert_eq!(p.vfs.file_attributes(destination_path), Some(0x20));
        let status = p.vfs.file_status(destination_path).unwrap();
        assert_eq!(status.accessed, Some(10));
        assert_eq!(status.modified, Some(20));
        assert_eq!(status.created, Some(30));

        p.vfs.insert(source_path, vec![9]);
        assert_eq!(
            call(&mut p, "_wrename", &[source, destination]),
            Ok(u32::MAX as u64)
        );
        assert_eq!(p.vfs.get(destination_path).unwrap(), &[1, 2, 3]);
        let errno = super::errno_cell(&mut p);
        assert_eq!(p.rd32(errno).unwrap(), 17); // EEXIST
    }

    #[test]
    fn setvbuf_accepts_existing_unbuffered_streams_and_stops_for_buffered_modes() {
        let mut p = process();
        let iob = super::ensure_iob(&mut p).unwrap();
        let stdout = iob + super::FILE_SIZE;
        assert_eq!(call(&mut p, "setvbuf", &[stdout, 0, 2, 0]), Ok(0));
        assert!(matches!(
            call(&mut p, "setvbuf", &[stdout, 0, 0, 1024]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("setvbuf") && name.contains("full/line buffering")
        ));
        assert_eq!(
            call(&mut p, "setvbuf", &[0xdead, 0, 2, 0]),
            Ok(u32::MAX as u64)
        );
    }

    #[test]
    fn user_matherr_and_os_dependent_imports_never_claim_translated_success() {
        let mut p = process();
        assert_eq!(call(&mut p, "__setusermatherr", &[0]), Ok(0));
        assert!(matches!(
            call(&mut p, "__setusermatherr", &[0x1234]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "crt" && name.contains("__setusermatherr")
        ));
        for (symbol, explanation) in [
            ("tmpnam", "temporary-directory policy"),
            ("tmpfile", "delete-on-close VFS handle"),
            ("_popen", "shell-pipe process creation"),
            ("_pclose", "shell-pipe wait status"),
            ("_wspawnv", "child-process creation"),
            ("fscanf", "stream position"),
            ("__dllonexit", "per-module callback table"),
            ("_invoke_watson", "Windows fail-fast"),
            ("_crt_debugger_hook", "debugger trap"),
            ("?terminate@@YAXXZ", "terminate handler"),
            (
                "?_type_info_dtor_internal_method@type_info@@QAEXXZ",
                "type_info destructor",
            ),
        ] {
            assert!(
                matches!(
                    call(&mut p, symbol, &[]),
                    Err(Stop::Unimplemented { dll, name })
                        if dll == "crt" && name.contains(symbol) && name.contains(explanation)
                ),
                "{symbol} must stop with its translation gap exposed"
            );
        }
    }
}
