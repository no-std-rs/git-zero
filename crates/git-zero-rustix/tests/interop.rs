#[test]
fn real_git_interoperability_matrix() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("tests/interop.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_git-zero")])
        .output()
        .expect("python3 is required for the Git interoperability suite");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}
