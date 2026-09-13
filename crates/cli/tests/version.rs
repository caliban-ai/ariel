use std::process::Command;

#[test]
fn ariel_prints_its_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_ariel"))
        .arg("--version")
        .output()
        .expect("run ariel --version");

    assert!(
        out.status.success(),
        "ariel --version exited {}",
        out.status
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert_eq!(
        stdout.trim(),
        format!("ariel {}", env!("CARGO_PKG_VERSION"))
    );
}
