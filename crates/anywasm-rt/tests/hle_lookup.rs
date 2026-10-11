use anywasm_rt::hle;

#[test]
fn implemented_current_directory_and_unimplemented_filesystem_mutators_are_reported_truthfully() {
    for name in ["SetCurrentDirectoryA", "SetCurrentDirectoryW", "GetCurrentDirectoryA", "GetCurrentDirectoryW"] {
        assert!(hle::lookup("kernel32", name).is_some(), "{name} should be implemented");
    }
    for name in [
        "CreateDirectoryA",
        "CreateDirectoryW",
        "SetFileAttributesA",
        "SetFileAttributesW",
    ] {
        assert!(
            hle::lookup("kernel32", name).is_none(),
            "{name} must stop as an unimplemented import while it does not change runtime state"
        );
    }
}
