use std::path::PathBuf;
use std::process::Command;

#[test]
fn run_allows_a_guest_callback_to_outlive_the_old_internal_slice() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = repository.join("fixtures/callback_budget.exe");
    let output = Command::new(env!("CARGO_BIN_EXE_anywasm"))
        .arg("run")
        .arg(fixture)
        .arg("--dir")
        .arg(repository.join("fixtures"))
        .arg("--max-steps")
        .arg("20000000")
        .output()
        .expect("run the callback-budget PE fixture");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
