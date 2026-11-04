use std::process::Command;

#[test]
fn version_flag_prints_the_cargo_toml_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_kbx"))
        .arg("--version")
        .output()
        .expect("kbx should run");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        concat!("kbx ", env!("CARGO_PKG_VERSION"))
    );
}
