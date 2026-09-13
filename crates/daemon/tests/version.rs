use std::process::Command;

#[test]
fn arield_prints_its_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_arield"))
        .arg("--version")
        .output()
        .expect("run arield --version");

    assert!(
        out.status.success(),
        "arield --version exited {}",
        out.status
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert_eq!(
        stdout.trim(),
        format!("arield {}", env!("CARGO_PKG_VERSION"))
    );
}
