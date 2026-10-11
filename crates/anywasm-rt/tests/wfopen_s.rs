use anywasm_rt::{Process, Stop, Vfs};

fn fixture() -> Vec<u8> {
    let path = format!("{}/../../fixtures/wfopen_s_test.exe", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn wide_open_uses_utf16_paths_and_vfs_byte_streams() {
    let mut vfs = Vfs::new();
    vfs.insert("wfopen_s_test.exe", fixture());
    let mut process = Process::new(vfs);
    process.load_exe("wfopen_s_test.exe", None).unwrap();

    let stop = process.run_to_end(10_000_000);
    assert_eq!(stop, Stop::Exit(0));
    assert_eq!(process.vfs.get("caf\u{e9}.txt").map(Vec::as_slice), Some(&b"vfs!!"[..]));
}
