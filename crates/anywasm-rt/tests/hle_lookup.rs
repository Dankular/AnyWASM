use anywasm_rt::hle;

#[test]
fn filesystem_mutators_are_not_claimed_until_they_change_runtime_state() {
    for name in [
        "SetCurrentDirectoryA",
        "SetCurrentDirectoryW",
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
