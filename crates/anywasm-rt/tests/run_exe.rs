//! End-to-end: load real Windows x64 executables (built by fixtures/build.sh) and run them.
use anywasm_rt::{Process, Stop, Vfs};

fn fixture(name: &str) -> Vec<u8> {
    let p = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn run(name: &str) -> (Stop, String, String, Process) {
    let mut vfs = Vfs::new();
    vfs.insert(name, fixture(name));
    let mut p = Process::new(vfs);
    p.load_exe(name, None).unwrap_or_else(|e| panic!("load {name}: {e}"));
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
    assert_eq!(stop, Stop::Unimplemented { dll: "kernel32".into(), name: "CreateToolhelp32Snapshot".into() });
    assert_eq!(out, "before\n", "output up to the failing call is preserved");
    assert_eq!(p.unimplemented_log, vec![("kernel32".to_string(), "CreateToolhelp32Snapshot".to_string())]);
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
    p.load_exe(name, Some(cmdline)).unwrap_or_else(|e| panic!("load {name}: {e}"));
    let stop = p.run_to_end(500_000_000);
    let out = String::from_utf8_lossy(&p.take_stdout()).into_owned();
    let err = String::from_utf8_lossy(&p.take_stderr()).into_owned();
    (stop, out, err, p)
}

#[test]
fn mingw_crt_program_runs_with_real_msvcrt_imports() {
    let (stop, out, err, p) = run_with("crt_test.exe", "\"C:\\crt_test.exe\" first \"second arg\"");
    assert_eq!(stop, Stop::Exit(7), "stdout:\n{out}\nstderr:\n{err}\nunimplemented: {:?}", p.unimplemented_log);
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
