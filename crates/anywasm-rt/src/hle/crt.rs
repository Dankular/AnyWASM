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
const MSVC_STD_COUT_OBJECT: &str = "msvcp:std::cout:object";
const MSVC_STD_CERR_OBJECT: &str = "msvcp:std::cerr:object";
const MSVC_STD_COUT_BUFFER: &str = "msvcp:std::cout:streambuf";
const MSVC_STD_CERR_BUFFER: &str = "msvcp:std::cerr:streambuf";
const MSVC_STD_OSTREAM_VTABLE: &str = "msvcp:std::ostream:vtable";
const MSVC_STD_STREAMBUF_VTABLE: &str = "msvcp:std::streambuf:vtable";
const MSVC_STD_OSTREAM_VBTABLE: &str = "msvcp:std::ostream:vbtable";
const MSVC_STD_LOCALE: &str = "msvcp:std::locale";
const ONEXIT_INITIAL_CAPACITY: u64 = 32;
const ONEXIT_MAX_ENTRIES: u64 = 1 << 20;

/// Data imports (variables): returns the guest address of the cell and initialises it.
pub fn data_import(p: &mut Process, name: &str, size: u64) -> Result<u64, Stop> {
    if matches!(name, MSVC_STD_COUT_SYMBOL | MSVC_STD_CERR_SYMBOL) {
        return msvc_standard_ostream_import(p, name);
    }

    let a = data_cell(p, name, size);
    match name {
        "__argc" => {
            let argc = p.extra.argc.max(1) as u64;
            p.wr32(a, argc as u32)?;
        }
        "__argv" | "_acmdln" | "_pgmptr" => {
            ensure_args(p)?;
            let v = if name == "__argv" {
                p.extra.argv
            } else if name == "_pgmptr" {
                p.extra.argv
            } else {
                p.extra.cmdline_a
            };
            p.wr64(a, v)?;
        }
        "_iob" => {
            ensure_iob(p)?;
            // `_iob` is an array of FILE, not a pointer: callers take its address.
            return Ok(p.extra.iob);
        }
        "_environ" => {
            p.wr64(a, 0)?;
        }
        "__mb_cur_max" => {
            p.wr32(a, 1)?;
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
    let arr = kernel32::alloc_block(p, 8 * (args.len() as u64 + 1), true);
    for (i, a) in args.iter().enumerate() {
        let s = kernel32::alloc_block(p, a.len() as u64 + 1, true);
        p.write_cstr(s, a)?;
        p.wr64(arr + 8 * i as u64, s)?;
    }
    p.extra.argv = arr;
    if p.extra.cmdline_a == 0 {
        let c = p.cmdline.clone();
        let a = kernel32::alloc_block(p, c.len() as u64 + 1, true);
        p.write_cstr(a, &c)?;
        p.extra.cmdline_a = a;
    }
    Ok(())
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
    match &mut f.kind {
        FileKind::Stdout => p.stdout.extend_from_slice(data),
        FileKind::Stderr => p.stderr.extend_from_slice(data),
        FileKind::Stdin => return Ok(0),
        FileKind::Vfs { path, pos, write } => {
            if !*write || p.vfs.is_readonly(path) {
                f.err = true;
                return Ok(0);
            }
            let (path, at) = (path.clone(), *pos);
            let Some(file) = p.vfs.get_mut(&path) else {
                f.err = true;
                return Ok(0);
            };
            if file.len() < at + data.len() {
                file.resize(at + data.len(), 0);
            }
            file[at..at + data.len()].copy_from_slice(data);
            *pos = at.saturating_add(data.len());
        }
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
        FileKind::Vfs { path, pos, .. } => {
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

/// The base `basic_streambuf<char>::sync()` succeeds without doing anything.
fn msvc_basic_streambuf_char_sync(_p: &mut Process) -> R {
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
        (9 * 8, "__anywasm_standard_streambuf_xsputn"),
        (13 * 8, "__anywasm_standard_streambuf_sync"),
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
    let sync = p
        .thunk_for("msvcp", "__anywasm_basic_streambuf_char_sync", true)
        .expect("creating the MSVCP basic_streambuf sync thunk");
    p.wr64(vtable + 13 * 8, sync)?;
    let lock_noop = p
        .thunk_for("msvcp", "__anywasm_basic_streambuf_char_lock_noop", true)
        .expect("creating the MSVCP basic_streambuf lock no-op thunk");
    p.wr64(vtable + 8, lock_noop)?;
    p.wr64(vtable + 16, lock_noop)?;

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

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "__anywasm_basic_streambuf_char_lock_noop"
        | "?_Lock@?$basic_streambuf@DU?$char_traits@D@std@@@std@@UEAAXXZ"
        | "?_Unlock@?$basic_streambuf@DU?$char_traits@D@std@@@std@@UEAAXXZ" => {
            msvc_basic_streambuf_char_lock_noop
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
        "__anywasm_basic_streambuf_char_sync" => msvc_basic_streambuf_char_sync,
        "__anywasm_standard_streambuf_char_overflow" => msvc_standard_streambuf_char_overflow,
        "__anywasm_standard_streambuf_xsputn" => msvc_standard_streambuf_xsputn,
        "__anywasm_standard_streambuf_sync" => msvc_standard_streambuf_sync,
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
        "__set_app_type" | "_set_app_type" | "_set_fmode" | "_setmode" | "_set_new_mode" | "_set_invalid_parameter_handler" | "_set_abort_behavior" | "_set_error_mode" | "__setusermatherr" | "_fpreset" | "_configthreadlocale" | "_set_purecall_handler"
        | "_crt_at_quick_exit" | "_initialize_narrow_environment" | "_initialize_wide_environment" | "__security_init_cookie" | "_seh_filter_exe" | "_controlfp_s" | "_clearfp" | "_statusfp" | "__lconv_init" | "_get_osfhandle"
        | "_set_thread_local_invalid_parameter_handler" | "_CrtSetReportMode" | "_CrtSetReportFile" | "_invalid_parameter_noinfo" | "_invalid_parameter_noinfo_noreturn" => |_| Ok(0),
        "_controlfp" => |_| Ok(0x9001f),
        "_configure_narrow_argv" | "_configure_wide_argv" => |p| {
            ensure_args(p)?;
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
            p.wr64(a, p.extra.argv)?;
            Ok(a)
        },
        "__p__environ" | "__p__wenviron" => |p| {
            let a = data_cell(p, "_environ", 8);
            Ok(a)
        },
        "__p__acmdln" | "__p__pgmptr" => |p| {
            ensure_args(p)?;
            let a = data_cell(p, "_acmdln", 8);
            p.wr64(a, p.extra.cmdline_a)?;
            Ok(a)
        },
        "_get_initial_narrow_environment" | "_get_initial_wide_environment" => |p| Ok(data_cell(p, "_environ", 8)),
        "__getmainargs" | "__wgetmainargs" => |p| {
            ensure_args(p)?;
            let (pargc, pargv, penv) = (p.arg(0), p.arg(1), p.arg(2));
            p.wr32(pargc, p.extra.argc)?;
            p.wr64(pargv, p.extra.argv)?;
            let env = data_cell(p, "_environ", 8);
            p.wr64(penv, env)?;
            Ok(0)
        },
        "_get_narrow_winmain_command_line" => |p| {
            ensure_args(p)?;
            Ok(p.extra.cmdline_a)
        },
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
        "abort" | "_abort" => |p| {
            p.stderr.extend_from_slice(b"abnormal program termination\n");
            p.finish_exit(3).map(|_| 0)
        },
        "_amsg_exit" | "_wassert" | "_assert" => |p| {
            let msg = if p.arg(0) > 0x1000 { p.read_cstr(p.arg(0)).unwrap_or_default() } else { String::new() };
            p.stderr.extend_from_slice(format!("assertion/runtime error {msg}\n").as_bytes());
            p.finish_exit(3).map(|_| 0)
        },
        "_errno" => |p| Ok(errno_cell(p)),
        "__doserrno" => |p| Ok(errno_cell(p) + 4),
        "_lock_locales" => lock_locales,
        "_unlock_locales" => unlock_locales,
        "_lock" | "_unlock" | "_lock_file" | "_unlock_file" | "_mlock" | "_munlock" => |_| Ok(0),
        "_getpid" => |_| Ok(0x1000),
        "___lc_codepage_func" | "__lc_codepage_func" => |_| Ok(1252),
        "___lc_locale_name_func" => |_| Ok(0),
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
        "signal" => |_| Ok(0),
        "raise" => |p| Err(Stop::Crash(format!("raise({})", p.arg(0)))),
        "_XcptFilter" => |_| Ok(0),
        "__C_specific_handler" => crate::exception::c_specific_handler,
        "_except_handler4_common" => crate::exception32::except_handler4_common,
        "__CxxFrameHandler3" => crate::msvc_eh::frame_handler3,
        "__CxxFrameHandler4" => crate::msvc_eh::frame_handler4,
        "_CxxThrowException" => crate::msvc_eh::cxx_throw_exception,
        "__std_terminate" | "terminate" => crate::msvc_eh::std_terminate,
        "_register_thread_local_exe_atexit_callback" => |_| Ok(0),
        "__current_exception" => |p| crate::msvc_eh::current_cpp_exception_cell(p, false),
        "__current_exception_context" => |p| crate::msvc_eh::current_cpp_exception_cell(p, true),
        "_setjmp" | "_setjmpex" | "setjmp" | "__intrinsic_setjmp" => setjmp,
        "longjmp" => longjmp,
        "_Query_perf_counter" => msvc_query_perf_counter,
        "_Query_perf_frequency" => |_| Ok(kernel32::QUERY_PERFORMANCE_FREQUENCY),
        "_beginthreadex" => kernel32::create_thread,
        "_beginthread" => |p| Err(Stop::Unimplemented { dll: "crt".into(), name: "_beginthread (cdecl thread wrapper is not implemented yet)".into() }.record(p)),
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
        "strspn" | "strcspn" | "strpbrk" => |p| {
            Err(Stop::Unimplemented { dll: "crt".into(), name: "strspn/strcspn/strpbrk".into() }.record(p))
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
        "isxdigit" => |p| Ok((p.arg(0) < 256 && (p.arg(0) as u8).is_ascii_hexdigit()) as u64),
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
        "fflush" | "_flushall" | "setvbuf" | "setbuf" | "_fflush_nolock" => |_| Ok(0),
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
            let path = crate::vfs::absolute(&n, &p.current_directory);
            Ok(if p.vfs.remove(&path) { 0 } else { u64::MAX })
        },
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
        "_putenv" | "system" => |_| Ok(0),
        "_fileno" => |p| Ok(match p.extra.files.get(&p.arg(0)) {
            Some(CFile { kind: FileKind::Stdin, .. }) => 0,
            Some(CFile { kind: FileKind::Stdout, .. }) => 1,
            Some(CFile { kind: FileKind::Stderr, .. }) => 2,
            _ => 3,
        }),
        "_isatty" => |p| Ok((p.arg(0) < 3) as u64),
        "_write" => |p| {
            let (fd, buf, n) = (p.arg(0), p.arg(1), p.arg(2) as usize);
            let d = p.read_bytes(buf, n)?;
            match fd {
                1 => p.stdout.extend_from_slice(&d),
                2 => p.stderr.extend_from_slice(&d),
                _ => return Ok(u64::MAX),
            }
            Ok(n as u64)
        },
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

fn errno_cell(p: &mut Process) -> u64 {
    if p.extra.errno_cell == 0 {
        p.extra.errno_cell = data_cell(p, "errno", 16);
    }
    p.extra.errno_cell
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

fn onexit_pointer_width(p: &Process) -> u64 {
    if p.cpu.mode == CpuMode::X86_32 {
        4
    } else {
        8
    }
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

fn write_onexit_table_fields(
    p: &mut Process,
    table: u64,
    fields: [u64; 3],
) -> Result<(), Stop> {
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
    if mode.starts_with('w') {
        p.vfs.insert(&path, Vec::new());
    } else if !exists {
        if mode.starts_with('a') {
            p.vfs.insert(&path, Vec::new());
        } else {
            return Ok(0);
        }
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
                write,
            },
            eof: false,
            err: false,
            ungot: None,
        },
    );
    Ok(fp)
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
// jmp_buf layout used here (our own, the program treats it as opaque): rbx rbp rdi rsi r12..r15
// rsp rip xmm6..xmm15.

fn setjmp(p: &mut Process) -> R {
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

fn longjmp(p: &mut Process) -> R {
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
