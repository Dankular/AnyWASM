//! C-ABI surface of the AnyWASM runtime for WebAssembly hosts.
//!
//! One process per module instance. Typical host sequence:
//! `aw_alloc` -> copy bundle bytes in -> `aw_load` -> loop `aw_run(steps)` while it returns 0,
//! draining `aw_stdout_*` / `aw_stderr_*` between calls.
//! The module imports `anywasm.run_emitted_block` and `anywasm.yield_slice`; `web/anywasm.js`
//! supplies the JIT trampoline and resumes Asyncify-suspended guest callbacks between slices.
//!
//! Status codes from `aw_run`: 0 running (step budget used), 1 exited, 2 stopped at an
//! unimplemented import, 3 guest crash, 4 a frame was presented (drain the GPU stream, then call
//! again), 5 the guest is idle waiting for window input (push input, then call again). Details via `aw_message_*` and `aw_exit_code`.

use anywasm_rt::{Bundle, Process, Stop};
mod shared_context;
mod shared_snapshot;

struct State {
    p: Process,
    message: Vec<u8>,
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    gpu: Vec<u8>,
    pending_stop: Option<Stop>,
}

static mut STATE: Option<State> = None;

#[allow(static_mut_refs)]
fn state() -> Option<&'static mut State> {
    // SAFETY: WebAssembly instances are single threaded; the host never calls re-entrantly.
    unsafe { STATE.as_mut() }
}

#[no_mangle]
pub extern "C" fn aw_alloc(n: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(n.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `ptr`/`n` must come from a matching `aw_alloc(n)`.
#[no_mangle]
pub unsafe extern "C" fn aw_free(ptr: *mut u8, n: usize) {
    drop(Vec::from_raw_parts(ptr, 0, n.max(1)));
}

/// Load a bundle. Returns 0 on success, 1 on failure (see `aw_message_*`).
///
/// # Safety
/// `ptr` must point to `len` readable bytes.
#[no_mangle]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn aw_load(ptr: *const u8, len: usize) -> i32 {
    let bytes = std::slice::from_raw_parts(ptr, len);
    let res = Bundle::from_bytes(bytes).and_then(|b| {
        let (vfs, exe, cmd, cwd) = b.into_vfs_with_cwd();
        let mut p = Process::new(vfs);
        if let Some(cwd) = cwd {
            let cwd = anywasm_rt::vfs::absolute(&cwd, "C:\\");
            if !p.vfs.is_dir(&cwd) {
                return Err(format!(
                    "bundle current directory is not present in its VFS: {cwd}"
                ));
            }
            p.current_directory = cwd;
        }
        p.load_exe(&exe, cmd.as_deref())?;
        Ok(p)
    });
    match res {
        Ok(p) => {
            STATE = Some(State {
                p,
                message: Vec::new(),
                exit_code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
                gpu: Vec::new(),
                pending_stop: None,
            });
            0
        }
        Err(e) => {
            STATE = Some(State {
                p: Process::new(anywasm_rt::Vfs::new()),
                message: e.into_bytes(),
                exit_code: 0,
                stdout: vec![],
                stderr: vec![],
                gpu: vec![],
                pending_stop: None,
            });
            1
        }
    }
}

/// Attach an `AGP1` index after loading a process. The package payload stays in the host.
///
/// # Safety
/// `ptr` must point to `len` readable bytes containing the complete asset header and index.
#[no_mangle]
pub unsafe extern "C" fn aw_assets_attach(ptr: *const u8, len: usize) -> i32 {
    let Some(s) = state() else { return 1 };
    let bytes = std::slice::from_raw_parts(ptr, len);
    match s.p.vfs.register_asset_index(bytes) {
        Ok(()) => {
            s.message.clear();
            0
        }
        Err(message) => {
            s.message = message.into_bytes();
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn aw_run(steps: u64) -> i32 {
    let Some(s) = state() else { return 3 };
    if let Some(stop) = s.pending_stop.take() {
        s.stdout.extend(s.p.take_stdout());
        s.stderr.extend(s.p.take_stderr());
        s.gpu.extend(std::mem::take(&mut s.p.extra.gpu.buf));
        return match stop {
            Stop::Unimplemented { dll, name } => {
                s.message =
                    format!("unimplemented import {dll}!{name} from input callback").into_bytes();
                2
            }
            Stop::Crash(message) => {
                s.message = format!("input callback crashed: {message}").into_bytes();
                3
            }
            other => {
                s.message = format!("input callback stopped guest: {other:?}").into_bytes();
                3
            }
        };
    }
    let r = s.p.run(steps);
    s.stdout.extend(s.p.take_stdout());
    s.stderr.extend(s.p.take_stderr());
    // Move GPU/window commands to the host-visible buffer (appended: the host clears after reading).
    let g = std::mem::take(&mut s.p.extra.gpu.buf);
    s.gpu.extend(g);
    match r {
        Stop::StepLimit => 0,
        Stop::Exit(c) => {
            s.exit_code = c as i32;
            1
        }
        Stop::Unimplemented { dll, name } => {
            s.message = format!("unimplemented import {dll}!{name}").into_bytes();
            2
        }
        Stop::Crash(m) => {
            s.message = m.into_bytes();
            3
        }
        Stop::Frame => 4,
        Stop::Idle => 5,
        Stop::ThreadExit(code) => {
            s.message = format!("internal thread exit escaped its worker context (code {code})")
                .into_bytes();
            3
        }
        Stop::GuestUnwind(_) => {
            s.message = b"internal guest unwind escaped its exception dispatcher".to_vec();
            3
        }
    }
}

/// Set the minimum number of leading MOV instructions required for dynamic block emission.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn aw_jit_set_min_prefix(min_prefix: u32) {
    if let Some(s) = state() {
        s.p.block_cache.set_jit_min_prefix(min_prefix as usize);
    }
}

#[no_mangle]
pub extern "C" fn aw_exit_code() -> i32 {
    state().map(|s| s.exit_code).unwrap_or(-1)
}

/// Retired guest instruction count (low 53 bits are exact when read as a JS number).
#[no_mangle]
pub extern "C" fn aw_icount() -> u64 {
    state().map(|s| s.p.cpu.icount).unwrap_or(0)
}

/// Retired guest instruction count across all guest threads.
#[no_mangle]
pub extern "C" fn aw_retired_instructions() -> u64 {
    state().map(|s| s.p.retired_instructions).unwrap_or(0)
}

/// Copy a stable little-endian snapshot of the active CPU for JIT differential tests.
///
/// Layout: mode (u32; 0=x86-32, 1=x86-64), GPRs, RIP/RFLAGS/FS/GS bases, XMM and YMM-high
/// registers, MXCSR, eight f64 x87 stack slots as their raw u64 bits, x87 TOP/CW/SW/TW as u32,
/// then TSC and icount. Returns 776 bytes, or zero when the output buffer is too small or no
/// process is loaded. This export exists only in builds using the `jit-test-api` feature.
///
/// # Safety
/// `ptr` must point to `capacity` writable bytes.
#[cfg(feature = "jit-test-api")]
#[no_mangle]
pub unsafe extern "C" fn aw_test_cpu_snapshot(ptr: *mut u8, capacity: usize) -> usize {
    const SNAPSHOT_SIZE: usize = 776;
    if ptr.is_null() || capacity < SNAPSHOT_SIZE {
        return 0;
    }
    let Some(s) = state() else { return 0 };
    let cpu = &s.p.cpu;
    let out = std::slice::from_raw_parts_mut(ptr, SNAPSHOT_SIZE);
    let mut cursor = 0;
    macro_rules! put {
        ($value:expr) => {{
            let bytes = $value.to_le_bytes();
            out[cursor..cursor + bytes.len()].copy_from_slice(&bytes);
            cursor += bytes.len();
        }};
    }
    put!(cpu.mode as u32);
    for value in cpu.gpr {
        put!(value);
    }
    for value in [cpu.rip, cpu.rflags, cpu.fs_base, cpu.gs_base] {
        put!(value);
    }
    for value in cpu.xmm {
        put!(value);
    }
    for value in cpu.ymm_hi {
        put!(value);
    }
    put!(cpu.mxcsr);
    for value in cpu.fpu_st {
        put!(value.to_bits());
    }
    for value in [
        cpu.fpu_top as u32,
        cpu.fpu_cw as u32,
        cpu.fpu_sw as u32,
        cpu.fpu_tw as u32,
    ] {
        put!(value);
    }
    for value in [cpu.tsc, cpu.icount] {
        put!(value);
    }
    debug_assert_eq!(cursor, SNAPSHOT_SIZE);
    SNAPSHOT_SIZE
}

/// Copy all mapped guest pages in ascending guest-page order. Each record contains the guest page
/// number (u64 LE), permissions (u8), and all 4096 bytes. With a null or undersized output buffer,
/// returns the required byte count without writing. This export exists only with `jit-test-api`.
///
/// # Safety
/// When `ptr` is non-null and the buffer is large enough, it must point to `capacity` writable
/// bytes.
#[cfg(feature = "jit-test-api")]
#[no_mangle]
pub unsafe extern "C" fn aw_test_memory_snapshot(ptr: *mut u8, capacity: usize) -> usize {
    let Some(s) = state() else { return 0 };
    let snapshot = s.p.mem.test_snapshot_mapped_pages();
    if ptr.is_null() || capacity < snapshot.len() {
        return snapshot.len();
    }
    std::ptr::copy_nonoverlapping(snapshot.as_ptr(), ptr, snapshot.len());
    snapshot.len()
}

/// Read a VFS asset range into a linear-memory buffer to exercise Asyncify-backed host I/O.
/// This export exists only in builds using the `jit-test-api` feature.
///
/// # Safety
/// `path_ptr` must point to `path_len` readable bytes and `destination` must point to `len`
/// writable bytes.
#[cfg(feature = "jit-test-api")]
#[no_mangle]
pub unsafe extern "C" fn aw_test_asset_read(
    path_ptr: *const u8,
    path_len: u32,
    offset: u64,
    len: u32,
    destination: *mut u8,
) -> i32 {
    let Some(s) = state() else { return 1 };
    if (path_ptr.is_null() && path_len != 0) || (destination.is_null() && len != 0) {
        s.message = b"invalid asset read test buffer".to_vec();
        return 1;
    }
    let path_bytes = if path_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(path_ptr, path_len as usize)
    };
    let path = match std::str::from_utf8(path_bytes) {
        Ok(path) => path,
        Err(_) => {
            s.message = b"asset read test path is not UTF-8".to_vec();
            return 1;
        }
    };
    let bytes = match s.p.vfs.read_range(path, offset, len as usize) {
        Ok(bytes) => bytes,
        Err(error) => {
            s.message = format!("asset read test failed: {error:?}").into_bytes();
            return 1;
        }
    };
    if !bytes.is_empty() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len());
    }
    0
}

macro_rules! buf_api {
    ($ptr:ident, $len:ident, $clear:ident, $field:ident) => {
        #[no_mangle]
        pub extern "C" fn $ptr() -> *const u8 {
            state()
                .map(|s| s.$field.as_ptr())
                .unwrap_or(std::ptr::null())
        }
        #[no_mangle]
        pub extern "C" fn $len() -> usize {
            state().map(|s| s.$field.len()).unwrap_or(0)
        }
        #[no_mangle]
        pub extern "C" fn $clear() {
            if let Some(s) = state() {
                s.$field.clear();
            }
        }
    };
}
buf_api!(aw_stdout_ptr, aw_stdout_len, aw_stdout_clear, stdout);
buf_api!(aw_stderr_ptr, aw_stderr_len, aw_stderr_clear, stderr);
buf_api!(aw_message_ptr, aw_message_len, aw_message_clear, message);
buf_api!(aw_gpu_ptr, aw_gpu_len, aw_gpu_clear, gpu);

/// Deliver a browser input event (see `hle::user32::input_event` for the kinds).
#[no_mangle]
pub extern "C" fn aw_input(kind: u32, a: i64, b: i64, c: i64) {
    if let Some(s) = state() {
        s.p.queue_input_event(kind, a, b, c);
    }
}

/// Advance guest time by host elapsed 100 ns ticks after a yielded browser frame.
#[no_mangle]
pub extern "C" fn aw_advance_time_100ns(ticks: u64) {
    if let Some(s) = state() {
        s.p.advance_time_100ns(ticks);
    }
}

/// Queue bytes for the guest's stdin.
///
/// # Safety
/// `ptr` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn aw_stdin_push(ptr: *const u8, len: usize) {
    if let Some(s) = state() {
        s.p.stdin
            .extend(std::slice::from_raw_parts(ptr, len).iter().copied());
    }
}
