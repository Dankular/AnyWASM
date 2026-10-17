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

use anywasm_rt::{hle::user32, Bundle, Process, Stop};
mod shared_context;
mod shared_snapshot;

struct State {
    p: Process,
    staged_entry: Option<String>,
    loaded: bool,
    message: Vec<u8>,
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    gpu: Vec<u8>,
    pending_stop: Option<Stop>,
}

static mut STATE: Option<State> = None;
const DEBUG_RSP_INDEX: usize = 4;

fn new_state(p: Process, staged_entry: Option<String>, loaded: bool, message: Vec<u8>) -> State {
    State {
        p,
        staged_entry,
        loaded,
        message,
        exit_code: 0,
        stdout: Vec::new(),
        stderr: Vec::new(),
        gpu: Vec::new(),
        pending_stop: None,
    }
}

fn validate_staged_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains(':')
        || path.contains('\0')
    {
        return Err(format!(
            "staged path must be nonempty and relative: {path:?}"
        ));
    }
    if path
        .split(['/', '\\'])
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(format!("staged path contains an unsafe component: {path}"));
    }
    Ok(())
}

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
        let hooks = b.hooks.clone();
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
        p.install_native_hooks(&hooks)?;
        Ok(p)
    });
    match res {
        Ok(p) => {
            STATE = Some(new_state(p, None, true, Vec::new()));
            0
        }
        Err(e) => {
            STATE = Some(new_state(
                Process::new(anywasm_rt::Vfs::new()),
                None,
                false,
                e.into_bytes(),
            ));
            1
        }
    }
}

/// Begin a direct-folder load and register its in-memory `AGP1` index before PE imports resolve.
/// Returns 0 on success and 1 on failure (see `aw_message_*`).
///
/// # Safety
/// Both pointers must refer to their stated readable byte ranges.
#[no_mangle]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn aw_load_begin(
    entry_ptr: *const u8,
    entry_len: usize,
    index_ptr: *const u8,
    index_len: usize,
) -> i32 {
    let result = (|| {
        if entry_ptr.is_null() || index_ptr.is_null() {
            return Err("direct load received a null entry or index buffer".to_string());
        }
        let entry_bytes = std::slice::from_raw_parts(entry_ptr, entry_len);
        let entry = std::str::from_utf8(entry_bytes)
            .map_err(|_| "direct entry path is not UTF-8".to_string())?
            .to_string();
        validate_staged_path(&entry)?;
        let index = std::slice::from_raw_parts(index_ptr, index_len);
        let mut p = Process::new(anywasm_rt::Vfs::new());
        p.vfs.register_asset_index(index)?;
        Ok((entry, p))
    })();

    match result {
        Ok((entry, p)) => {
            STATE = Some(new_state(p, Some(entry), false, Vec::new()));
            0
        }
        Err(error) => {
            STATE = Some(new_state(
                Process::new(anywasm_rt::Vfs::new()),
                None,
                false,
                error.into_bytes(),
            ));
            1
        }
    }
}

/// Add one local boot image (the selected EXE or a guest DLL) during a staged direct load.
/// Returns 0 on success and 1 on failure (see `aw_message_*`).
///
/// # Safety
/// `path_ptr` and `data_ptr` must refer to their stated readable byte ranges.
#[no_mangle]
pub unsafe extern "C" fn aw_load_add_file(
    path_ptr: *const u8,
    path_len: usize,
    data_ptr: *const u8,
    data_len: usize,
) -> i32 {
    let Some(s) = state() else { return 1 };
    if s.loaded || s.staged_entry.is_none() {
        s.message = b"direct load is not accepting boot files".to_vec();
        return 1;
    }
    if path_ptr.is_null() || (data_ptr.is_null() && data_len != 0) {
        s.message = b"direct load received a null path or file buffer".to_vec();
        return 1;
    }
    let path_bytes = std::slice::from_raw_parts(path_ptr, path_len);
    let path = match std::str::from_utf8(path_bytes) {
        Ok(path) => path,
        Err(_) => {
            s.message = b"direct boot file path is not UTF-8".to_vec();
            return 1;
        }
    };
    if let Err(error) = validate_staged_path(path) {
        s.message = error.into_bytes();
        return 1;
    }
    if s.p.vfs.has_local_file(path) {
        s.message = format!("duplicate staged boot file path: {path}").into_bytes();
        return 1;
    }
    let data = if data_len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(data_ptr, data_len).to_vec()
    };
    s.p.vfs.insert(path, data);
    s.message.clear();
    0
}

/// Finish a staged direct load by mapping the selected EXE and resolving its imports.
/// Returns 0 on success and 1 on failure (see `aw_message_*`).
#[no_mangle]
pub extern "C" fn aw_load_finish() -> i32 {
    let Some(s) = state() else { return 1 };
    if s.loaded {
        s.message = b"process is already loaded".to_vec();
        return 1;
    }
    let Some(entry) = s.staged_entry.clone() else {
        s.message = b"direct load has not been started".to_vec();
        return 1;
    };
    let cwd = entry
        .rfind(['/', '\\'])
        .map(|separator| &entry[..separator])
        .unwrap_or("");
    let cwd = if cwd.is_empty() {
        "C:\\".to_string()
    } else {
        anywasm_rt::vfs::absolute(cwd, "C:\\")
    };
    if !s.p.vfs.is_dir(&cwd) {
        s.message =
            format!("direct current directory is not present in its VFS: {cwd}").into_bytes();
        return 1;
    }
    s.p.current_directory = cwd;
    match s.p.load_exe(&entry, None) {
        Ok(()) => {
            s.staged_entry = None;
            s.loaded = true;
            s.message.clear();
            0
        }
        Err(error) => {
            s.message = error.into_bytes();
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

/// Mount a drive-qualified host install path onto the direct folder VFS root.
///
/// # Safety
/// `ptr` must point to `len` readable UTF-8 bytes.
#[no_mangle]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn aw_set_vfs_root_alias(ptr: *const u8, len: usize) -> i32 {
    let Some(s) = state() else { return 1 };
    if ptr.is_null() {
        s.message = b"VFS root alias received a null path".to_vec();
        return 1;
    }
    let root = match std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) {
        Ok(root) => root.trim_end_matches(['\\', '/']),
        Err(_) => {
            s.message = b"VFS root alias is not UTF-8".to_vec();
            return 1;
        }
    };
    let current_suffix =
        s.p.current_directory
            .strip_prefix("C:\\")
            .or_else(|| s.p.current_directory.strip_prefix("c:\\"))
            .unwrap_or("")
            .to_owned();
    if let Err(error) = s.p.vfs.set_root_alias(root) {
        s.message = error.into_bytes();
        return 1;
    }
    s.p.current_directory = if current_suffix.is_empty() {
        root.to_owned()
    } else {
        format!("{root}\\{current_suffix}")
    };
    s.message.clear();
    0
}

#[no_mangle]
pub extern "C" fn aw_run(steps: u64) -> i32 {
    let Some(s) = state() else { return 3 };
    if !s.loaded {
        s.message = b"process load has not been finished".to_vec();
        return 3;
    }
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
            Stop::AbiUnimplemented { dll, name } => {
                s.message = format!(
                    "x86 ABI signature not verified for import {dll}!{name} from input callback"
                )
                .into_bytes();
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
        Stop::AbiUnimplemented { dll, name } => {
            s.message =
                format!("x86 ABI signature not verified for import {dll}!{name}").into_bytes();
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
        Stop::GuestExceptionTransfer => {
            s.message = b"internal nested exception transfer escaped its dispatcher".to_vec();
            3
        }
        Stop::GuestCatch32(_) => {
            s.message =
                b"internal PE32 C++ catch transfer escaped its exception dispatcher".to_vec();
            3
        }
        Stop::GuestUnwind32(_) => {
            s.message = b"internal PE32 unwind transfer escaped its exception dispatcher".to_vec();
            3
        }
        Stop::GuestSehTransfer32(_) => {
            s.message = b"internal PE32 SEH transfer escaped its exception dispatcher".to_vec();
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

/// Enable targeted GTA event volume diagnostics used by the browser run harness.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn aw_set_gta_eventvol_trace(enabled: u32) {
    if let Some(s) = state() {
        s.p.trace_gta_eventvol = enabled != 0;
    }
}

/// Enable import call and argument tracing for browser diagnostics.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn aw_set_import_trace(enabled: u32) {
    if let Some(s) = state() {
        s.p.trace_imports = enabled != 0;
        s.p.trace_modules_only = false;
    }
}

/// Supply native host settings used by the translated SystemParametersInfo actions.
/// Returns 0 after a process is loaded, or 1 when no loaded process can receive the value.
#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn aw_set_host_system_parameters(
    cursor_shadow: u32,
    mouse_trails: u32,
    mouse_sonar: u32,
    wheel_scroll_lines: u32,
) -> i32 {
    let Some(s) = state() else { return 1 };
    if !s.loaded {
        s.message = b"host system parameters arrived before process load completed".to_vec();
        return 1;
    }
    s.p.system_parameters.cursor_shadow = Some(cursor_shadow != 0);
    s.p.system_parameters.mouse_trails = Some(mouse_trails);
    s.p.system_parameters.mouse_sonar = Some(mouse_sonar != 0);
    s.p.system_parameters.wheel_scroll_lines = Some(wheel_scroll_lines);
    s.message.clear();
    0
}

/// Install a scoped, read-only Windows Sims registry snapshot after process load.
/// Returns 0 on success; invalid snapshots leave the process registry unchanged.
///
/// # Safety
/// `ptr` must reference `len` readable bytes allocated by `aw_alloc`.
#[no_mangle]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn aw_set_host_registry_snapshot(ptr: *const u8, len: usize) -> i32 {
    let Some(s) = state() else { return 1 };
    if !s.loaded {
        s.message = b"host registry snapshot arrived before process load completed".to_vec();
        return 1;
    }
    if ptr.is_null() || len == 0 || len > 32 * 1024 * 1024 {
        s.message = b"host registry snapshot has an invalid buffer".to_vec();
        return 1;
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    match anywasm_rt::registry::load_host_snapshot(&mut s.p, bytes) {
        Ok(()) => {
            s.message.clear();
            0
        }
        Err(error) => {
            s.message = format!("host registry snapshot rejected: {error}").into_bytes();
            1
        }
    }
}

/// Current guest instruction pointer for targeted browser diagnostics.
#[no_mangle]
pub extern "C" fn aw_debug_rip() -> u64 {
    state().map(|s| s.p.cpu.rip).unwrap_or(0)
}

/// Current guest thread ID for targeted browser diagnostics.
#[no_mangle]
pub extern "C" fn aw_debug_thread_id() -> u32 {
    state().map(|s| s.p.current_thread_id).unwrap_or(0)
}

/// Number of guest thread contexts, including the active CPU context.
#[no_mangle]
pub extern "C" fn aw_debug_thread_count() -> u32 {
    state()
        .map(|s| s.p.guest_threads.len() as u32 + u32::from(!s.p.current_thread_saved))
        .unwrap_or(0)
}

/// Guest thread ID at a stable sorted index; the active context is listed first.
#[no_mangle]
pub extern "C" fn aw_debug_thread_at(index: u32) -> u32 {
    let Some(s) = state() else { return 0 };
    let p = &s.p;
    if !p.current_thread_saved {
        if index == 0 {
            return p.current_thread_id;
        }
        let mut ids: Vec<u32> = p
            .guest_threads
            .keys()
            .copied()
            .filter(|id| *id != p.current_thread_id)
            .collect();
        ids.sort_unstable();
        return ids.get((index - 1) as usize).copied().unwrap_or(0);
    }
    let mut ids: Vec<u32> = p.guest_threads.keys().copied().collect();
    ids.sort_unstable();
    ids.get(index as usize).copied().unwrap_or(0)
}

/// Packed state for a guest thread: state in bits 0..7, wait kind in 8..15, guest flag in bit 16.
/// State values: 1 ready, 2 running, 3 suspended, 4 blocked, 5 exited.
/// Wait kinds: 1 handle, 2 multiple handles, 3 sleep, 4 critical section, 5 SRW lock, 6 condition.
#[no_mangle]
pub extern "C" fn aw_debug_thread_state(thread_id: u32) -> u32 {
    use anywasm_rt::process::{ThreadState, ThreadWait};
    let Some(s) = state() else { return 0 };
    let p = &s.p;
    let (thread_state, wait_kind, is_guest) =
        if !p.current_thread_saved && thread_id == p.current_thread_id {
            (&p.current_thread_state, 0, p.current_thread_is_guest)
        } else if let Some(thread) = p.guest_threads.get(&thread_id) {
            let wait_kind = match &thread.state {
                ThreadState::Blocked(ThreadWait::Handle { .. }) => 1,
                ThreadState::Blocked(ThreadWait::Multiple { .. }) => 2,
                ThreadState::Blocked(ThreadWait::Sleep { .. }) => 3,
                ThreadState::Blocked(ThreadWait::CriticalAcquire { .. }) => 4,
                ThreadState::Blocked(ThreadWait::SrwAcquire { .. }) => 5,
                ThreadState::Blocked(ThreadWait::Condition { .. }) => 6,
                _ => 0,
            };
            (&thread.state, wait_kind, thread.is_guest)
        } else {
            return 0;
        };
    let state_code = match thread_state {
        ThreadState::Ready => 1,
        ThreadState::Running => 2,
        ThreadState::Suspended => 3,
        ThreadState::Blocked(_) => 4,
        ThreadState::Exited(_) => 5,
    };
    state_code | (wait_kind << 8) | (u32::from(is_guest) << 16)
}

/// Saved or active guest instruction pointer for targeted thread diagnostics.
#[no_mangle]
pub extern "C" fn aw_debug_thread_rip(thread_id: u32) -> u64 {
    let Some(s) = state() else { return 0 };
    let p = &s.p;
    if !p.current_thread_saved && thread_id == p.current_thread_id {
        p.cpu.rip
    } else {
        p.guest_threads
            .get(&thread_id)
            .map(|thread| thread.cpu.rip)
            .unwrap_or(0)
    }
}

/// Handle awaited by a blocked guest thread, or zero when it is not waiting on a handle.
#[no_mangle]
pub extern "C" fn aw_debug_thread_wait_handle(thread_id: u32) -> u64 {
    use anywasm_rt::process::ThreadWait;
    let Some(s) = state() else { return 0 };
    let p = &s.p;
    let thread_state = if !p.current_thread_saved && thread_id == p.current_thread_id {
        &p.current_thread_state
    } else if let Some(thread) = p.guest_threads.get(&thread_id) {
        &thread.state
    } else {
        return 0;
    };
    match thread_state {
        anywasm_rt::process::ThreadState::Blocked(ThreadWait::Handle { handle, .. }) => *handle,
        anywasm_rt::process::ThreadState::Blocked(ThreadWait::Multiple { handles, .. }) => {
            handles.first().copied().unwrap_or(0)
        }
        _ => 0,
    }
}

/// Handle type and signal state: 1 event, 2 mutex, 3 semaphore, 4 thread, 0 unknown.
#[no_mangle]
pub extern "C" fn aw_debug_handle_state(handle: u64) -> u32 {
    use anywasm_rt::process::Handle;
    let Some(s) = state() else { return 0 };
    match s.p.handles.get(&handle) {
        Some(Handle::Event { signaled, manual }) => {
            1 | (u32::from(*signaled) << 8) | (u32::from(*manual) << 9)
        }
        Some(Handle::Mutex { owner, .. }) => 2 | (u32::from(owner.is_none()) << 8),
        Some(Handle::Semaphore { count, .. }) => 3 | ((*count).min(0xffff) << 8),
        Some(Handle::Thread { exit_code, .. }) => 4 | (u32::from(exit_code.is_some()) << 8),
        _ => 0,
    }
}

/// Stack pointer for an active or saved guest thread.
#[no_mangle]
pub extern "C" fn aw_debug_thread_sp(thread_id: u32) -> u64 {
    let Some(s) = state() else { return 0 };
    let p = &s.p;
    if !p.current_thread_saved && thread_id == p.current_thread_id {
        p.cpu.gpr[DEBUG_RSP_INDEX]
    } else {
        p.guest_threads
            .get(&thread_id)
            .map(|thread| thread.cpu.gpr[DEBUG_RSP_INDEX])
            .unwrap_or(0)
    }
}

/// 32-bit word from an active or saved guest thread's stack.
#[no_mangle]
pub extern "C" fn aw_debug_thread_stack_word(thread_id: u32, index: u32) -> u32 {
    let Some(s) = state() else { return 0 };
    let p = &s.p;
    let sp = if !p.current_thread_saved && thread_id == p.current_thread_id {
        p.cpu.gpr[DEBUG_RSP_INDEX]
    } else if let Some(thread) = p.guest_threads.get(&thread_id) {
        thread.cpu.gpr[DEBUG_RSP_INDEX]
    } else {
        return 0;
    };
    p.rd32(sp.wrapping_add(index as u64 * 4)).unwrap_or(0)
}

/// Current guest GPR as a 32-bit value for targeted x86 diagnostics.
#[no_mangle]
pub extern "C" fn aw_debug_gpr(index: u32) -> u32 {
    state()
        .and_then(|s| s.p.cpu.gpr.get(index as usize).copied())
        .unwrap_or(0) as u32
}

#[no_mangle]
pub extern "C" fn aw_debug_rflags() -> u32 {
    state().map(|s| s.p.cpu.rflags as u32).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn aw_debug_guest_u16(address: u32) -> u32 {
    state()
        .and_then(|s| s.p.mem.read_u16(address as u64).ok())
        .unwrap_or(0) as u32
}

#[no_mangle]
pub extern "C" fn aw_debug_guest_u32(address: u32) -> u32 {
    state()
        .and_then(|s| s.p.mem.read_u32(address as u64).ok())
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn aw_debug_tick_ms() -> u64 {
    state().map(|s| s.p.tick_ms).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn aw_debug_window_queue_len() -> u32 {
    state()
        .map(|s| s.p.extra.win.queue.len() as u32)
        .unwrap_or(0)
}

/// Win32 message field: 0 hwnd, 1 message, 2 wParam, 3 lParam.
#[no_mangle]
pub extern "C" fn aw_debug_window_message(index: u32, field: u32) -> u32 {
    let Some(s) = state() else { return 0 };
    let Some(message) = s.p.extra.win.queue.get(index as usize) else {
        return 0;
    };
    match field {
        0 => message.hwnd as u32,
        1 => message.msg as u32,
        2 => message.wparam as u32,
        3 => message.lparam as u32,
        _ => 0,
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

/// Advance guest time by host elapsed 100 ns ticks after a yielded frame or idle wait.
#[no_mangle]
pub extern "C" fn aw_advance_time_100ns(ticks: u64) {
    if let Some(s) = state() {
        s.p.advance_time_100ns(ticks);
    }
}

/// Milliseconds until the current guest thread's next user32 timer, or u32::MAX if none exists.
#[no_mangle]
pub extern "C" fn aw_next_timer_delay_ms() -> u32 {
    state()
        .and_then(|s| user32::next_timer_delay_ms(&s.p))
        .unwrap_or(u32::MAX)
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
