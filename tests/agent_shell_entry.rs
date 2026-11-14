//! Exercise the actual client executables: unit-test shell execution uses a
//! different executable and cannot protect the desktop subprocess contract.
use std::{
    io::Write,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn clients() -> Vec<&'static str> {
    vec![
        #[cfg(feature = "tui")]
        env!("CARGO_BIN_EXE_nole"),
        #[cfg(feature = "gui")]
        env!("CARGO_BIN_EXE_nole-gui"),
    ]
}

fn run_helper(client: &str, arguments: &[&str], input: &str) -> Output {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    let mut child = Command::new(client)
        .arg("--agent-shell-helper")
        .args(arguments)
        .current_dir(directory.path())
        .env("HOME", directory.path())
        .env("NOLE_DIR", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("{client} did not return shell output: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        !root.exists(),
        "shell subprocess opened the client workspace"
    );
    output
}

#[test]
fn client_shell_subprocesses_return_stdout_stderr_and_exit_status() {
    for client in clients() {
        let output = run_helper(
            client,
            &[
                "command",
                "printf 'client shell'; printf 'command error' >&2; exit 7",
            ],
            "",
        );
        assert_eq!(output.status.code(), Some(7), "{client}: {output:?}");
        assert_eq!(output.stdout, b"client shell");
        assert_eq!(output.stderr, b"command error");
    }
}

#[test]
fn client_interactive_shells_accept_input_and_return_results() {
    for client in clients() {
        let output = run_helper(
            client,
            &["interactive", "/tmp"],
            "printf 'terminal result\\n'\nexit 9\n",
        );
        assert_eq!(output.status.code(), Some(9), "{client}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("terminal result"));
    }
}
