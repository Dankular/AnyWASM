//! End-to-end: load real Windows x64 executables (built by fixtures/build.sh) and run them.
use anywasm_cpu::mem::{PAGE_SIZE, PROT_RWX};
use anywasm_cpu::cpu::{RAX, RSP, CpuMode};
use anywasm_rt::{Process, Stop, Vfs};
use anywasm_rt::process::{EXIT_MAGIC, TEB_ADDR};

fn minimal_pe32(code: &[u8]) -> Vec<u8> {
    assert!(code.len() <= 0x200);
    let mut pe = vec![0u8; 0x400];
    let put16 = |b: &mut [u8], at: usize, v: u16| b[at..at + 2].copy_from_slice(&v.to_le_bytes());
    let put32 = |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    pe[..2].copy_from_slice(b"MZ");
    put32(&mut pe, 0x3c, 0x80);
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    let coff = 0x84;
    put16(&mut pe, coff, 0x014c);
    put16(&mut pe, coff + 2, 1);
    put16(&mut pe, coff + 16, 224);
    put16(&mut pe, coff + 18, 0x0102);
    let opt = coff + 20;
    put16(&mut pe, opt, 0x010b);
    put32(&mut pe, opt + 16, 0x1000);
    put32(&mut pe, opt + 28, 0x0040_0000);
    put32(&mut pe, opt + 32, 0x1000);
    put32(&mut pe, opt + 36, 0x200);
    put32(&mut pe, opt + 56, 0x2000);
    put32(&mut pe, opt + 60, 0x200);
    put16(&mut pe, opt + 68, 3);
    put32(&mut pe, opt + 72, 0x100000);
    put32(&mut pe, opt + 76, 0x1000);
    put32(&mut pe, opt + 80, 0x100000);
    put32(&mut pe, opt + 84, 0x1000);
    put32(&mut pe, opt + 92, 16);
    let sec = opt + 224;
    pe[sec..sec + 5].copy_from_slice(b".text");
    put32(&mut pe, sec + 8, 0x200);
    put32(&mut pe, sec + 12, 0x1000);
    put32(&mut pe, sec + 16, 0x200);
    put32(&mut pe, sec + 20, 0x200);
    put32(&mut pe, sec + 36, 0x60000020);
    pe[0x200..0x200 + code.len()].copy_from_slice(code);
    pe
}

#[test]
fn pe32_x86_entry_executes_and_win32_import_uses_stdcall_stack_cleanup() {
    let mut vfs = Vfs::new();
    vfs.insert("x86_test.exe", minimal_pe32(&[0xc3]));
    let mut process = Process::new(vfs);
    process.load_exe("x86_test.exe", None).unwrap();
    assert_eq!(process.cpu.mode, CpuMode::X86_32);

    let stack = 0x7eff_0000;
    let thunk = process.thunk_for("kernel32", "SetLastError", true).unwrap();
    process.mem.write_u32(stack, 0x0040_1000).unwrap();
    process.mem.write_u32(stack + 4, 123).unwrap();
    process.mem.write_u32(stack + 8, EXIT_MAGIC as u32).unwrap();
    process.cpu.gpr[RSP] = stack;
    process.cpu.gpr[RAX] = 1;
    process.cpu.rip = thunk;

    assert_eq!(process.run_to_end(100), Stop::Exit(0));
    assert_eq!(process.mem.read_u32(TEB_ADDR + 0x34).unwrap(), 123);
    assert_eq!(process.cpu.gpr[RSP], stack + 12);
}

fn fixture(name: &str) -> Vec<u8> {
    let p = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn run(name: &str) -> (Stop, String, String, Process) {
    let mut vfs = Vfs::new();
    vfs.insert(name, fixture(name));
    let mut p = Process::new(vfs);
    p.load_exe(name, None)
        .unwrap_or_else(|e| panic!("load {name}: {e}"));
    let stop = p.run_to_end(200_000_000);
    let out = String::from_utf8_lossy(&p.take_stdout()).into_owned();
    let err = String::from_utf8_lossy(&p.take_stderr()).into_owned();
    (stop, out, err, p)
}

#[test]
fn hello_exe_prints_and_exits_zero() {
    let (stop, out, _, _) = run("hello.exe");
    assert_eq!(stop, Stop::Exit(0));
    assert_eq!(out, "Hello from a Windows PE running inside AnyWASM!\r\n");
}

#[test]
fn multibyte_to_wide_char_obeys_win32_lengths_and_buffer_contract() {
    let (stop, out, err, p) = run("multibyte_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; stderr: {err:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn lc_map_string_ex_handles_count_only_and_nul_terminated_strings() {
    let (stop, out, err, p) = run("lcmap_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; stderr: {err:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn current_directory_resolves_relative_win32_and_crt_file_paths() {
    let mut vfs = Vfs::new();
    vfs.insert("cwd_test.exe", fixture("cwd_test.exe"));
    vfs.insert("game\\assets\\config.txt", b"vfs!!".to_vec());
    let mut p = Process::new(vfs);
    p.load_exe("cwd_test.exe", None).unwrap();

    let stop = p.run_to_end(100_000_000);
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {:?}; stderr: {:?}; unimplemented: {:?}",
        p.take_stdout(),
        p.take_stderr(),
        p.unimplemented_log
    );
}

#[test]
fn create_thread_runs_finite_worker_with_isolated_tls_and_thread_identity() {
    let (stop, out, err, p) = run("thread_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; stderr: {err:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn cooperative_threads_resume_condition_variable_waiters_after_lock_reacquisition() {
    let (stop, out, err, p) = run("thread_sync_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; stderr: {err:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn static_tls_is_initialized_and_isolated_per_guest_thread() {
    let (stop, out, err, p) = run("static_tls_thread_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; stderr: {err:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn process_run_honors_block_instruction_budget_and_icount() {
    const CODE: u64 = 0x10000;
    let mut p = Process::new(Vfs::new());
    assert!(p.mem.map(CODE, PAGE_SIZE, PROT_RWX));
    p.mem.write_force(CODE, &[0x90, 0x90, 0xf4]).unwrap(); // nop; nop; hlt
    p.cpu.rip = CODE;

    assert_eq!(p.run(0), Stop::StepLimit);
    assert_eq!(p.cpu.icount, 0);
    assert_eq!(p.cpu.rip, CODE);

    assert_eq!(p.run(1), Stop::StepLimit);
    assert_eq!(p.cpu.icount, 1);
    assert_eq!(p.cpu.rip, CODE + 1);

    assert_eq!(p.run(1), Stop::StepLimit);
    assert_eq!(p.cpu.icount, 2);
    assert_eq!(p.cpu.rip, CODE + 2);

    assert!(matches!(p.run(1), Stop::Crash(message) if message.contains("Halt")));
    assert_eq!(p.cpu.icount, 2);
}

#[test]
fn process_run_redecodes_after_guest_writes_to_executable_memory() {
    const CODE: u64 = 0x10000;
    let mut code = vec![0x48, 0xb8]; // mov rax, CODE + 13
    code.extend_from_slice(&(CODE + 13).to_le_bytes());
    code.extend_from_slice(&[0xc6, 0x00, 0xf4, 0x90, 0xf4]); // mov byte [rax], hlt; nop; hlt

    let mut p = Process::new(Vfs::new());
    assert!(p.mem.map(CODE, PAGE_SIZE, PROT_RWX));
    p.mem.write_force(CODE, &code).unwrap();
    p.cpu.rip = CODE;

    assert!(matches!(p.run(4), Stop::Crash(message) if message.contains("Halt")));
    assert_eq!(p.cpu.icount, 2, "stale cached NOP must not execute");
    assert_eq!(p.cpu.rip, CODE + 13);
}

#[test]
fn initializes_empty_slist_header() {
    let (stop, out, _, _) = run("slist_test.exe");
    assert_eq!(stop, Stop::Exit(0), "stdout so far: {out:?}");
    assert_eq!(out, "");
}

#[test]
fn get_capture_returns_the_current_window_handle() {
    let (stop, out, _, _) = run("capture_test.exe");
    assert_eq!(stop, Stop::Exit(0), "stdout so far: {out:?}");
    assert_eq!(out, "");
}

#[test]
fn is_window_unicode_reports_the_window_class_character_mode() {
    let (stop, out, _, _) = run("iswindowunicode_test.exe");
    assert_eq!(stop, Stop::Exit(0), "stdout so far: {out:?}");
    assert_eq!(out, "");
}

#[test]
fn get_dlg_item_returns_direct_child_by_control_id() {
    let (stop, out, _, _) = run("getdlgitem_test.exe");
    assert_eq!(stop, Stop::Exit(0), "stdout so far: {out:?}");
    assert_eq!(out, "");
}

#[test]
fn device_notification_registration_returns_a_closable_handle() {
    let (stop, out, _, p) = run("device_notification_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn enum_display_devices_returns_virtual_adapter_and_monitor() {
    let (stop, out, _, p) = run("enumdisplaydevices_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn enum_display_settings_returns_virtual_modes() {
    let (stop, out, _, p) = run("enumdisplaysettings_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn enum_display_monitors_calls_guest_and_respects_clipping() {
    let (stop, out, _, p) = run("enumdisplaymonitors_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn window_properties_round_trip_and_remove_by_name() {
    let (stop, out, _, p) = run("windowprops_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn rectangle_helpers_update_signed_coordinates_and_intersections() {
    let (stop, out, _, p) = run("rect_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn shell_drag_accept_files_is_available_for_windows() {
    let (stop, out, _, p) = run("dragaccept_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn virtual_gdi_display_context_reports_display_caps() {
    let (stop, out, _, p) = run("gdiprobe_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn map_virtual_key_w_maps_common_us_layout_codes() {
    let (stop, out, _, p) = run("mapvirtualkey_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn to_unicode_uses_virtual_key_and_keyboard_state() {
    let (stop, out, _, p) = run("tounicode_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn ver_set_condition_mask_sets_version_comparison_fields() {
    let (stop, out, _, p) = run("verconditionmask_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn verify_version_info_w_compares_version_and_sets_win32_last_error() {
    let (stop, out, _, p) = run("verconditionmask_test.exe");
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stdout: {out:?}; unimplemented: {:?}",
        p.unimplemented_log
    );
    assert!(out.is_empty());
}

#[test]
fn path_file_exists_w_checks_files_and_directories_in_the_vfs() {
    let mut vfs = Vfs::new();
    vfs.insert(
        "pathfileexists_test.exe",
        fixture("pathfileexists_test.exe"),
    );
    vfs.insert("game\\data\\present.txt", vec![1]);
    let mut p = Process::new(vfs);
    p.current_directory = "C:\\game".into();
    p.load_exe("pathfileexists_test.exe", None).unwrap();

    let stop = p.run_to_end(100_000_000);
    assert_eq!(
        stop,
        Stop::Exit(0),
        "stderr: {:?}; unimplemented: {:?}",
        p.take_stderr(),
        p.unimplemented_log
    );
}

#[test]
fn reloc_exe_follows_relocated_pointer_tables() {
    let (stop, out, _, _) = run("reloc.exe");
    assert_eq!(stop, Stop::Exit(0));
    assert_eq!(out, "alpha beta gamma\r\n");
}

#[test]
fn compute_exe_heap_fpu_indirect_calls_and_getprocaddress() {
    let (stop, out, _, p) = run("compute.exe");
    assert_eq!(stop, Stop::Exit(0), "stdout so far: {out:?}");
    assert!(out.contains("primes<20000=2262\r\n"), "{out:?}");
    assert!(out.contains("sumsq=332833500\r\n"), "{out:?}");
    assert!(out.contains("ind=1342\r\n"), "{out:?}");
    assert!(out.contains("fp=609\r\n"), "{out:?}"); // 100 iterations of x*1.5+0.25, cross-checked in Python
    assert!(out.contains("gpa=ok\r\n"), "{out:?}");
    assert!(p.cpu.icount > 1_000_000, "icount {}", p.cpu.icount);
}

#[test]
fn unimplemented_imports_are_reported_by_name() {
    // unimpl.exe calls kernel32!CreateToolhelp32Snapshot, which the runtime does not implement.
    let (stop, out, _, p) = run("unimpl.exe");
    assert_eq!(
        stop,
        Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "CreateToolhelp32Snapshot".into()
        }
    );
    assert_eq!(
        out, "before\n",
        "output up to the failing call is preserved"
    );
    assert_eq!(
        p.unimplemented_log,
        vec![(
            "kernel32".to_string(),
            "CreateToolhelp32Snapshot".to_string()
        )]
    );
}

#[test]
fn d3dprobe_runs_through_d3d11_and_user32() {
    // Formerly stopped at the first missing import; now D3D11CreateDevice and MessageBoxA are implemented.
    let (stop, _, err, _) = run("d3dprobe.exe");
    assert_eq!(stop, Stop::Exit(0));
    assert!(err.contains("[MessageBox] probe: ok"), "{err:?}");
}

fn run_with(name: &str, cmdline: &str) -> (Stop, String, String, Process) {
    let mut vfs = Vfs::new();
    vfs.insert(name, fixture(name));
    let mut p = Process::new(vfs);
    p.load_exe(name, Some(cmdline))
        .unwrap_or_else(|e| panic!("load {name}: {e}"));
    let stop = p.run_to_end(500_000_000);
    let out = String::from_utf8_lossy(&p.take_stdout()).into_owned();
    let err = String::from_utf8_lossy(&p.take_stderr()).into_owned();
    (stop, out, err, p)
}

#[test]
fn mingw_crt_program_runs_with_real_msvcrt_imports() {
    let (stop, out, err, p) = run_with("crt_test.exe", "\"C:\\crt_test.exe\" first \"second arg\"");
    assert_eq!(
        stop,
        Stop::Exit(7),
        "stdout:\n{out}\nstderr:\n{err}\nunimplemented: {:?}",
        p.unimplemented_log
    );
    let expected = "argc=3 argv0_has_exe=1\n\
arg1=first\n\
arg2=second arg\n\
1 3 7 19 23 42 56 88\n\
\x203.14|ab    |0042|ff|1234567890123\n\
heap string! len=12\n\
realloc kept: heap string!\n\
sqrt=1.4142 pow=1024.0 sin=0.8415 floor=-3\n\
before longjmp\n\
after longjmp r=7\n\
read: line 1\n\
read: line 2\n\
lines=2 toupper=Q isdigit=1\n\
sscanf=3 x+y=46 word=hello\n\
atexit handler ran\n";
    assert_eq!(out, expected);
    assert_eq!(err, "to stderr\n");
}
