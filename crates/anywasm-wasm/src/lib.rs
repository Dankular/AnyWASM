//! C-ABI surface of the AnyWASM runtime for WebAssembly hosts.
//!
//! One process per module instance. Typical host sequence:
//! `aw_alloc` -> copy bundle bytes in -> `aw_load` -> loop `aw_run(steps)` while it returns 0,
//! draining `aw_stdout_*` / `aw_stderr_*` between calls.
//! The module imports `anywasm.run_emitted_block`; `web/anywasm.js` supplies the host trampoline
//! that compiles emitted blocks against this module's exported memory.
//!
//! Status codes from `aw_run`: 0 running (step budget used), 1 exited, 2 stopped at an
//! unimplemented import, 3 guest crash, 4 a frame was presented (drain the GPU stream, then call
//! again), 5 the guest is idle waiting for window input (push input, then call again). Details via `aw_message_*` and `aw_exit_code`.

use anywasm_rt::{Bundle, Process, Stop};

struct State {
    p: Process,
    message: Vec<u8>,
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    gpu: Vec<u8>,
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
                return Err(format!("bundle current directory is not present in its VFS: {cwd}"));
            }
            p.current_directory = cwd;
        }
        p.load_exe(&exe, cmd.as_deref())?;
        Ok(p)
    });
    match res {
        Ok(p) => {
            STATE = Some(State { p, message: Vec::new(), exit_code: 0, stdout: Vec::new(), stderr: Vec::new(), gpu: Vec::new() });
            0
        }
        Err(e) => {
            STATE = Some(State { p: Process::new(anywasm_rt::Vfs::new()), message: e.into_bytes(), exit_code: 0, stdout: vec![], stderr: vec![], gpu: vec![] });
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn aw_run(steps: u64) -> i32 {
    let Some(s) = state() else { return 3 };
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
            s.message = format!("internal thread exit escaped its worker context (code {code})").into_bytes();
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

macro_rules! buf_api {
    ($ptr:ident, $len:ident, $clear:ident, $field:ident) => {
        #[no_mangle]
        pub extern "C" fn $ptr() -> *const u8 {
            state().map(|s| s.$field.as_ptr()).unwrap_or(std::ptr::null())
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
        anywasm_rt::hle::user32::input_event(&mut s.p, kind, a, b, c);
    }
}

/// Queue bytes for the guest's stdin.
///
/// # Safety
/// `ptr` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn aw_stdin_push(ptr: *const u8, len: usize) {
    if let Some(s) = state() {
        s.p.stdin.extend(std::slice::from_raw_parts(ptr, len).iter().copied());
    }
}
