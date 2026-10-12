//! Parser tests against real PE32+ binaries built by `fixtures/build.sh`.
//! Expected values were cross-checked against `llvm-readobj` output.
use anywasm_pe::*;

fn load(name: &str) -> Vec<u8> {
    let p = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e} (run fixtures/build.sh)"))
}

#[test]
fn hello_headers() {
    let d = load("hello.exe");
    let pe = Pe::parse(&d).unwrap();
    assert_eq!(pe.machine, MACHINE_AMD64);
    assert!(pe.is_64);
    assert_eq!(pe.image_base, 0x1_4000_0000);
    assert_eq!(pe.entry_rva, 0x1000);
    assert_eq!(pe.subsystem, SUBSYSTEM_WINDOWS_CUI);
    assert!(pe.dynamic_base());
    assert!(!pe.is_dll());
    assert!(!pe.is_dotnet());
    let names: Vec<_> = pe.sections.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, [".text", ".rdata"]);
    assert_eq!(pe.sections[0].virtual_address, 0x1000);
    assert!(pe.sections[0].executable());
    assert!(!pe.sections[1].executable());
}

#[test]
fn hello_imports() {
    let d = load("hello.exe");
    let pe = Pe::parse(&d).unwrap();
    let imps = pe.imports().unwrap();
    assert_eq!(imps.len(), 1);
    assert_eq!(imps[0].dll, "kernel32.dll");
    let names: Vec<_> = imps[0].entries.iter().map(|e| e.name.to_string()).collect();
    assert_eq!(names, ["ExitProcess", "GetStdHandle", "WriteFile"]);
    // IAT slots are consecutive 8-byte entries starting at 0x2080 (per llvm-readobj).
    assert_eq!(imps[0].entries[0].iat_rva, 0x2080);
    assert_eq!(imps[0].entries[1].iat_rva, 0x2088);
    assert_eq!(imps[0].entries[2].iat_rva, 0x2090);
    assert!(pe.delay_imports().unwrap().is_empty());
}

#[test]
fn multi_dll_imports() {
    let d = load("d3dprobe.exe");
    let pe = Pe::parse(&d).unwrap();
    assert_eq!(pe.subsystem, SUBSYSTEM_WINDOWS_GUI);
    let dlls: Vec<_> = pe
        .imports()
        .unwrap()
        .into_iter()
        .map(|i| canonical_dll(&i.dll))
        .collect();
    assert_eq!(dlls, ["kernel32", "user32", "d3d11"]);
}

#[test]
fn relocations_present_and_in_range() {
    let d = load("reloc.exe");
    let pe = Pe::parse(&d).unwrap();
    let rel = pe.relocations().unwrap();
    assert!(!rel.is_empty(), "reloc fixture must carry base relocations");
    assert!(rel.iter().all(|r| r.kind == REL_BASED_DIR64));
    assert!(rel.iter().all(|r| r.rva < pe.size_of_image));
    // Every relocated slot must hold a VA inside the image at preferred base.
    for r in &rel {
        let v = pe.u64_at_rva(r.rva).unwrap();
        assert!(
            v >= pe.image_base && v < pe.image_base + pe.size_of_image as u64,
            "slot {:#x} = {v:#x}",
            r.rva
        );
    }
}

#[test]
fn no_exports_no_tls_in_exe() {
    let d = load("hello.exe");
    let pe = Pe::parse(&d).unwrap();
    assert!(pe.exports().unwrap().is_empty());
    assert!(pe.tls().unwrap().is_none());
    assert!(pe.tls_callbacks().unwrap().is_empty());
}

#[test]
fn bad_rva_is_error() {
    let d = load("hello.exe");
    let pe = Pe::parse(&d).unwrap();
    assert!(matches!(pe.u32_at_rva(0x00ff_0000), Err(Error::BadRva(_))));
}
