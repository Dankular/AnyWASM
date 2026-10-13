use anywasm_rt::hle;

#[test]
fn implemented_current_directory_and_directory_creation_imports_are_reported_truthfully() {
    for name in [
        "SetCurrentDirectoryA",
        "SetCurrentDirectoryW",
        "GetCurrentDirectoryA",
        "GetCurrentDirectoryW",
    ] {
        assert!(
            hle::lookup("kernel32", name).is_some(),
            "{name} should be implemented"
        );
    }
    for name in ["CreateDirectoryA", "CreateDirectoryW"] {
        assert!(
            hle::lookup("kernel32", name).is_some(),
            "{name} should be implemented"
        );
    }
    for name in ["SetFileAttributesA", "SetFileAttributesW"] {
        assert!(
            hle::lookup("kernel32", name).is_some(),
            "{name} should be implemented (virtual file attributes)"
        );
    }
}
