#![cfg(feature = "tui")]

use std::process::Command;

#[test]
fn informational_commands_preserve_the_workspace_directory() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    for argument in ["--version", "-V", "--help", "-h"] {
        let output = Command::new(env!("CARGO_BIN_EXE_nole"))
            .arg(argument)
            .env("NOLE_DIR", &root)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        if matches!(argument, "--version" | "-V") {
            assert_eq!(text.trim(), concat!("nole ", env!("CARGO_PKG_VERSION")));
        } else {
            assert!(text.contains("Usage: nole"));
        }
        assert!(!root.exists());
    }
}

#[test]
fn unknown_arguments_report_usage_before_opening_the_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    for argument in ["web", "--unknown"] {
        let output = Command::new(env!("CARGO_BIN_EXE_nole"))
            .arg(argument)
            .env("NOLE_DIR", &root)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("nole --help"));
        assert!(!root.exists());
    }
}
