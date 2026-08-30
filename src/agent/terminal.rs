//! Shared Agent PTY session and non-interactive Brush command runner.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::embedded_terminal::{EmbeddedTerminal, TerminalSnapshot};

use super::{shell_helper_command, NONINTERACTIVE_ENVIRONMENT};

const TERMINAL_ROWS: u16 = 24;
const INITIAL_COLS: u16 = 80;
const OUTPUT_LIMIT: usize = 1024 * 1024;
const SETTLE_INTERVAL: Duration = Duration::from_millis(150);
const POLL_INTERVAL: Duration = Duration::from_millis(40);
const MAX_SETTLE_WAIT: Duration = Duration::from_secs(2);

struct LimitedOutput {
    bytes: Vec<u8>,
    total_bytes: u64,
}

impl LimitedOutput {
    fn truncated(&self) -> bool {
        self.total_bytes > self.bytes.len() as u64
    }
}

impl AgentTerminalStatus {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Running => "running".to_string(),
            Self::Exited(code) => format!("exited {code}"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AgentTerminalSnapshot {
    pub(crate) title: String,
    pub(crate) status: AgentTerminalStatus,
    pub(crate) terminal: TerminalSnapshot,
}

/// One watcher sample from a live Agent PTY session.
pub(crate) struct TerminalWatchSample {
    pub(crate) offset: u64,
    pub(crate) data: Vec<u8>,
    pub(crate) exit_code: Option<u32>,
    pub(crate) exited: bool,
    pub(crate) screen: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentTerminalStatus {
    Running,
    Exited(u32),
}

struct AgentTerminalSession {
    id: String,
    title: String,
    terminal: EmbeddedTerminal,
    status: AgentTerminalStatus,
}

#[derive(Default)]
struct AgentTerminalState {
    next_id: u64,
    session: Option<AgentTerminalSession>,
    monitor_changed: bool,
    #[cfg(test)]
    monitor_override: Option<AgentTerminalSnapshot>,
}

#[derive(Clone, Default)]
pub(crate) struct AgentTerminalHandle {
    inner: Arc<Mutex<AgentTerminalState>>,
}

impl AgentTerminalHandle {
    pub(crate) fn is_running(&self) -> bool {
        let Ok(state) = self.inner.lock() else {
            return false;
        };
        if let Some(session) = state.session.as_ref() {
            return matches!(session.status, AgentTerminalStatus::Running);
        }
        #[cfg(test)]
        {
            state
                .monitor_override
                .as_ref()
                .is_some_and(|snapshot| matches!(snapshot.status, AgentTerminalStatus::Running))
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    pub(crate) fn poll_monitor_change(&self) -> bool {
        let Ok(mut state) = self.inner.lock() else {
            return false;
        };
        let status_changed = state
            .session
            .as_mut()
            .is_some_and(|session| refresh_status(session).unwrap_or(false));
        state.monitor_changed |= status_changed;
        std::mem::take(&mut state.monitor_changed)
    }

    pub(crate) fn open(&self, root: &Path, nole_root: &Path, command: &str) -> Result<String> {
        if command.contains('\0') {
            bail!("terminal command cannot contain NUL bytes");
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
        ensure_open_slot(&mut state)?;
        state.next_id = state.next_id.saturating_add(1);
        let id = format!("terminal-{}", state.next_id);
        let helper = shell_helper_command(nole_root)?;
        let output_path = nole_root
            .join("agent-session")
            .join("pty")
            .join(&id);
        let mut terminal =
            EmbeddedTerminal::spawn_command_with_raw_log(root, helper, output_path)?;
        terminal.resize(TERMINAL_ROWS, INITIAL_COLS)?;
        let mut bytes = command.as_bytes().to_vec();
        bytes.push(b'\r');
        terminal.write_raw(&bytes)?;

        state.session = Some(AgentTerminalSession {
            id: id.clone(),
            title: compact_title(command),
            terminal,
            status: AgentTerminalStatus::Running,
        });
        state.monitor_changed = true;
        Ok(id)
    }

    pub(crate) fn write(&self, session_id: &str, bytes: &[u8]) -> Result<()> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
        let status_changed = {
            let session = state
                .session
                .as_mut()
                .context("no active Agent terminal session")?;
            ensure_session(session, session_id)?;
            refresh_status(session)?
        };
        state.monitor_changed |= status_changed;
        let session = state.session.as_mut().expect("session was validated");
        if !matches!(session.status, AgentTerminalStatus::Running) {
            bail!("terminal session has exited");
        }
        session.terminal.write_raw(bytes)
    }

    /// Validate that `session_id` names the current running Agent PTY without
    /// reading or returning any terminal screen contents.
    pub(crate) fn ensure_running_session(&self, session_id: &str) -> Result<()> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
        let status_changed = {
            let session = state
                .session
                .as_mut()
                .context("no active Agent terminal session")?;
            ensure_session(session, session_id)?;
            refresh_status(session)?
        };
        state.monitor_changed |= status_changed;
        let session = state.session.as_ref().expect("session was validated");
        if !matches!(session.status, AgentTerminalStatus::Running) {
            bail!("terminal session has exited");
        }
        Ok(())
    }

    pub(crate) fn observation(&self, session_id: &str) -> Result<Value> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
        let (status_changed, observation) = {
            let session = state
                .session
                .as_mut()
                .context("no active Agent terminal session")?;
            ensure_session(session, session_id)?;
            let status_changed = refresh_status(session)?;
            (status_changed, observation_value(session)?)
        };
        state.monitor_changed |= status_changed;
        Ok(observation)
    }

    pub(crate) fn output_since(&self, session_id: &str, from: u64) -> Result<Value> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
        let (status_changed, value) = {
            let session = state
                .session
                .as_mut()
                .context("no active Agent terminal session")?;
            ensure_session(session, session_id)?;
            let status_changed = refresh_status(session)?;
            let tap = session.terminal.raw_tap();
            let tap = tap
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal output state poisoned"))?;
            let (cursor, bytes) = tap.read_since(from)?;
            let value = json!({
                "session_id": session.id,
                "status": session.status.label(),
                "cursor": cursor.to_string(),
                "output": String::from_utf8_lossy(&bytes),
            });
            (status_changed, value)
        };
        state.monitor_changed |= status_changed;
        Ok(value)
    }

    /// One watcher sample: new raw-stream bytes since `from`, the exit state,
    /// and the current screen. `None` when the session is unknown.
    pub(crate) fn sample_watch(&self, session_id: &str, from: u64) -> Option<TerminalWatchSample> {
        let mut state = self.inner.lock().ok()?;
        let session = state.session.as_mut()?;
        if session.id != session_id {
            return None;
        }
        let _ = refresh_status(session);
        let (exit_code, exited) = match &session.status {
            AgentTerminalStatus::Exited(code) => (Some(*code), true),
            _ => (None, false),
        };
        let data = session.terminal.raw_tap();
        let tap = data.lock().ok()?;
        let (offset, bytes) = tap.read_since(from).ok()?;
        drop(tap);
        let screen = session.terminal.snapshot().plain_text();
        Some(TerminalWatchSample {
            offset,
            data: bytes,
            exit_code,
            exited,
            screen,
        })
    }

    pub(crate) fn wait_until_settled(
        &self,
        session_id: &str,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        let deadline = Instant::now() + MAX_SETTLE_WAIT;
        let mut last = self.observation(session_id)?;
        let mut stable_since = Instant::now();
        loop {
            if cancelled.load(Ordering::Relaxed) {
                bail!("agent task cancelled");
            }
            if Instant::now() >= deadline || stable_since.elapsed() >= SETTLE_INTERVAL {
                return Ok(last);
            }
            std::thread::sleep(POLL_INTERVAL);
            let current = self.observation(session_id)?;
            if current != last {
                last = current;
                stable_since = Instant::now();
            }
        }
    }

    pub(crate) fn wait_for_change(
        &self,
        session_id: &str,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        let initial = self.observation(session_id)?;
        let deadline = Instant::now() + timeout;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                bail!("agent task cancelled");
            }
            let current = self.observation(session_id)?;
            if current != initial || Instant::now() >= deadline {
                return Ok(current);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    pub(crate) fn close_exited(&self, session_id: &str) -> Result<()> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
        let status_changed = {
            let session = state
                .session
                .as_mut()
                .context("no active Agent terminal session")?;
            ensure_session(session, session_id)?;
            refresh_status(session)?
        };
        state.monitor_changed |= status_changed;
        if matches!(
            state
                .session
                .as_ref()
                .expect("session was validated")
                .status,
            AgentTerminalStatus::Running
        ) {
            bail!("terminal session is still running");
        }
        state.session = None;
        state.monitor_changed = true;
        Ok(())
    }

    pub(crate) fn terminate(&self) {
        if let Ok(mut state) = self.inner.lock() {
            state.session = None;
            state.monitor_changed = true;
            #[cfg(test)]
            {
                state.monitor_override = None;
            }
        }
    }

    pub(crate) fn monitor_snapshot(&self, cols: u16) -> Option<AgentTerminalSnapshot> {
        let mut state = self.inner.lock().ok()?;
        #[cfg(test)]
        if let Some(snapshot) = &state.monitor_override {
            return matches!(snapshot.status, AgentTerminalStatus::Running)
                .then(|| snapshot.clone());
        }
        let session = state.session.as_mut()?;
        if !matches!(session.status, AgentTerminalStatus::Running) {
            return None;
        }
        let _ = session.terminal.resize(TERMINAL_ROWS, cols.max(1));
        Some(AgentTerminalSnapshot {
            title: session.title.clone(),
            status: session.status.clone(),
            terminal: session.terminal.snapshot(),
        })
    }

    #[cfg(test)]
    pub(crate) fn set_monitor_snapshot_for_test(&self, snapshot: AgentTerminalSnapshot) {
        if let Ok(mut state) = self.inner.lock() {
            state.monitor_override = Some(snapshot);
            state.monitor_changed = true;
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn open_process_for_test(&self, root: &Path, script: &str) -> Result<String> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
        ensure_open_slot(&mut state)?;
        state.next_id = state.next_id.saturating_add(1);
        let id = format!("terminal-{}", state.next_id);
        let mut command = portable_pty::CommandBuilder::new("/bin/sh");
        command.arg("-c");
        command.arg(script);
        let output_path = root.join("agent-session").join("pty").join(&id);
        let mut terminal =
            EmbeddedTerminal::spawn_command_with_raw_log(root, command, output_path)?;
        terminal.resize(TERMINAL_ROWS, INITIAL_COLS)?;
        state.session = Some(AgentTerminalSession {
            id: id.clone(),
            title: compact_title(script),
            terminal,
            status: AgentTerminalStatus::Running,
        });
        state.monitor_changed = true;
        Ok(id)
    }
}

fn ensure_open_slot(state: &mut AgentTerminalState) -> Result<()> {
    let Some(session) = state.session.as_mut() else {
        return Ok(());
    };
    state.monitor_changed |= refresh_status(session)?;
    if matches!(session.status, AgentTerminalStatus::Running) {
        bail!("an Agent terminal session is already running");
    }
    Ok(())
}

fn ensure_session(session: &AgentTerminalSession, session_id: &str) -> Result<()> {
    if session.id == session_id {
        Ok(())
    } else {
        bail!("unknown terminal session {session_id}")
    }
}

fn refresh_status(session: &mut AgentTerminalSession) -> Result<bool> {
    if matches!(session.status, AgentTerminalStatus::Running) {
        if let Some(status) = session.terminal.try_wait()? {
            session.status = AgentTerminalStatus::Exited(status.exit_code());
            return Ok(true);
        }
    }
    Ok(false)
}

fn observation_value(session: &AgentTerminalSession) -> Result<Value> {
    let tap = session.terminal.raw_tap();
    let cursor = tap
        .lock()
        .map_err(|_| anyhow::anyhow!("terminal output state poisoned"))?
        .end_offset()?;
    Ok(json!({
        "session_id": session.id,
        "status": session.status.label(),
        "screen": session.terminal.snapshot().plain_text(),
        "cursor": cursor.to_string(),
    }))
}

fn compact_title(command: &str) -> String {
    let title = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = title.chars();
    let compact = chars.by_ref().take(48).collect::<String>();
    if chars.next().is_some() {
        format!("{compact}...")
    } else {
        compact
    }
}

pub(crate) fn resolve_shell_cwd(root: &Path, input: Option<&str>) -> Result<PathBuf> {
    let path = match input.map(str::trim).filter(|value| !value.is_empty()) {
        None => root.to_path_buf(),
        Some("~") => dirs::home_dir().context("home directory is unavailable")?,
        Some(value) if value.starts_with("~/") || value.starts_with("~\\") => dirs::home_dir()
            .context("home directory is unavailable")?
            .join(&value[2..]),
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        }
    };
    let path = std::fs::canonicalize(&path)
        .with_context(|| format!("resolving shell working directory {}", path.display()))?;
    if !path.is_dir() {
        bail!(
            "shell working directory is not a directory: {}",
            path.display()
        );
    }
    Ok(path)
}

pub(crate) fn run_noninteractive_shell(
    root: &Path,
    command: &str,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<Value> {
    run_shell_with_deadline(root, command, Some(timeout), cancelled)
}

/// Run a backgrounded shell command with no deadline: it ends when the
/// process exits or the job's cancellation flag is set — nothing else.
pub(crate) fn run_noninteractive_shell_untimed(
    root: &Path,
    command: &str,
    cancelled: &AtomicBool,
) -> Result<Value> {
    run_shell_with_deadline(root, command, None, cancelled)
}

fn run_shell_with_deadline(
    root: &Path,
    command: &str,
    timeout: Option<Duration>,
    cancelled: &AtomicBool,
) -> Result<Value> {
    if command.contains('\0') {
        bail!("shell command cannot contain NUL bytes");
    }
    // The helper re-invokes this executable; under `cargo test` the current
    // exe is the test harness, which rejects the flag. Test builds spawn the
    // shell directly so background-job tests exercise the real pipeline.
    let mut process = if cfg!(test) {
        let mut process = Command::new("/bin/sh");
        process.arg("-c").arg(command);
        process
    } else {
        let executable = std::env::current_exe().context("locating the Nole executable")?;
        let mut process = Command::new(executable);
        process
            .arg("--agent-shell-helper")
            .arg("command")
            .arg(command);
        process
    };
    process
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_noninteractive_process(&mut process);
    let mut child = process.spawn().context("starting Brush command")?;

    let stdout = child.stdout.take().context("capturing shell stdout")?;
    let stderr = child.stderr.take().context("capturing shell stderr")?;
    let stdout_reader = std::thread::spawn(move || read_limited(stdout));
    let stderr_reader = std::thread::spawn(move || read_limited(stderr));
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let status = loop {
        if cancelled.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("agent task cancelled");
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "shell command timed out after {} seconds",
                timeout.expect("deadline implies a timeout").as_secs()
            );
        }
        if let Some(status) = child.try_wait().context("waiting for Brush command")? {
            break status;
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("shell stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("shell stderr reader panicked"))??;
    Ok(shell_output_value(
        status.code().unwrap_or(1),
        stdout,
        stderr,
    ))
}

fn shell_output_value(exit_code: i32, stdout: LimitedOutput, stderr: LimitedOutput) -> Value {
    let stdout_truncated = stdout.truncated();
    let stderr_truncated = stderr.truncated();
    let mut result = json!({
        "exit_code": exit_code,
        "stdout": String::from_utf8_lossy(&stdout.bytes),
        "stderr": String::from_utf8_lossy(&stderr.bytes),
        "truncated": stdout_truncated || stderr_truncated,
        "output_limit_bytes_per_stream": OUTPUT_LIMIT,
        "stdout_bytes": stdout.total_bytes,
        "stdout_returned_bytes": stdout.bytes.len(),
        "stdout_truncated": stdout_truncated,
        "stderr_bytes": stderr.total_bytes,
        "stderr_returned_bytes": stderr.bytes.len(),
        "stderr_truncated": stderr_truncated,
    });
    if stdout_truncated || stderr_truncated {
        result["warning"] = json!(format!(
            "Output truncated: stdout returned {} of {} bytes; stderr returned {} of {} bytes (limit: {} bytes per stream).",
            stdout.bytes.len(),
            stdout.total_bytes,
            stderr.bytes.len(),
            stderr.total_bytes,
            OUTPUT_LIMIT,
        ));
    }
    result
}

fn configure_noninteractive_process(command: &mut Command) {
    command.stdin(Stdio::null());
    for (name, value) in NONINTERACTIVE_ENVIRONMENT {
        command.env(name, value);
    }
}

fn read_limited(mut reader: impl Read) -> Result<LimitedOutput> {
    let mut output = Vec::new();
    let mut total_bytes = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(count as u64);
        let remaining = OUTPUT_LIMIT.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    Ok(LimitedOutput {
        bytes: output,
        total_bytes,
    })
}

pub(crate) fn terminal_input_bytes(
    text: Option<&str>,
    submit: bool,
    key: Option<&str>,
) -> Result<Vec<u8>> {
    match (text, key) {
        (Some(text), None) => {
            let mut bytes = text.as_bytes().to_vec();
            if submit {
                bytes.push(b'\r');
            }
            Ok(bytes)
        }
        (None, Some(key)) if !submit => terminal_key_bytes(key),
        _ => bail!("provide either text (optionally submitted) or one key"),
    }
}

fn terminal_key_bytes(key: &str) -> Result<Vec<u8>> {
    if let Some(letter) = key
        .strip_prefix("ctrl-")
        .filter(|letter| letter.len() == 1)
        .and_then(|letter| letter.bytes().next())
        .filter(u8::is_ascii_lowercase)
    {
        return Ok(vec![letter - b'a' + 1]);
    }
    let bytes: &[u8] = match key {
        "enter" => b"\r",
        "tab" => b"\t",
        "escape" => b"\x1b",
        "backspace" => b"\x7f",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "delete" => b"\x1b[3~",
        "page-up" => b"\x1b[5~",
        "page-down" => b"\x1b[6~",
        _ => bail!("unsupported terminal key {key}"),
    };
    Ok(bytes.to_vec())
}

pub(crate) fn terminal_input_display(
    text: Option<&str>,
    submit: bool,
    key: Option<&str>,
) -> Result<String> {
    match (text, key) {
        (Some(text), None) => Ok(if submit {
            format!("{text} ↵")
        } else {
            text.to_string()
        }),
        (None, Some(key)) if !submit => Ok(match key {
            "enter" => "Enter".to_string(),
            "tab" => "Tab".to_string(),
            "escape" => "Escape".to_string(),
            key if key.starts_with("ctrl-") => key.to_uppercase(),
            key => key.to_string(),
        }),
        _ => bail!("provide either text (optionally submitted) or one key"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_output_reports_total_and_retained_bytes() {
        let input = vec![b'x'; OUTPUT_LIMIT + 37];
        let output = read_limited(std::io::Cursor::new(input)).unwrap();
        assert_eq!(output.bytes.len(), OUTPUT_LIMIT);
        assert_eq!(output.total_bytes, (OUTPUT_LIMIT + 37) as u64);
        assert!(output.truncated());
    }

    #[cfg(unix)]
    #[test]
    fn untimed_shell_runs_past_the_timed_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let cancelled = AtomicBool::new(false);
        // The timed variant kills a 2s sleep at its 1s deadline...
        let timed = run_noninteractive_shell(
            directory.path(),
            "sleep 2",
            Duration::from_secs(1),
            &cancelled,
        );
        assert!(timed.is_err());
        assert!(timed
            .unwrap_err()
            .to_string()
            .contains("timed out after 1 seconds"));
        // ...while the untimed variant lets the same command finish.
        let untimed =
            run_noninteractive_shell_untimed(directory.path(), "sleep 2", &cancelled).unwrap();
        assert_eq!(untimed["exit_code"], 0);
    }

    #[test]
    fn shell_output_has_per_stream_truncation_metadata_and_warning() {
        let result = shell_output_value(
            0,
            LimitedOutput {
                bytes: vec![b'x'; OUTPUT_LIMIT],
                total_bytes: OUTPUT_LIMIT as u64 + 9,
            },
            LimitedOutput {
                bytes: b"warning".to_vec(),
                total_bytes: 7,
            },
        );
        assert_eq!(result["truncated"], true);
        assert_eq!(result["stdout_bytes"], OUTPUT_LIMIT as u64 + 9);
        assert_eq!(result["stdout_returned_bytes"], OUTPUT_LIMIT);
        assert_eq!(result["stdout_truncated"], true);
        assert_eq!(result["stderr_bytes"], 7);
        assert_eq!(result["stderr_returned_bytes"], 7);
        assert_eq!(result["stderr_truncated"], false);
        assert_eq!(result["output_limit_bytes_per_stream"], OUTPUT_LIMIT);
        assert!(result["warning"]
            .as_str()
            .is_some_and(|warning| warning.starts_with("Output truncated:")));
    }

    #[test]
    fn shell_output_omits_warning_when_complete() {
        let result = shell_output_value(
            0,
            LimitedOutput {
                bytes: b"ok".to_vec(),
                total_bytes: 2,
            },
            LimitedOutput {
                bytes: Vec::new(),
                total_bytes: 0,
            },
        );
        assert_eq!(result["truncated"], false);
        assert!(result.get("warning").is_none());
    }

    #[test]
    fn truncated_utf8_is_rendered_lossily_without_panicking() {
        let mut input = vec![b'x'; OUTPUT_LIMIT - 1];
        input.extend_from_slice("é".as_bytes());
        let output = read_limited(std::io::Cursor::new(input)).unwrap();
        assert!(output.truncated());
        assert!(String::from_utf8_lossy(&output.bytes).ends_with('\u{fffd}'));
    }

    #[test]
    fn terminal_input_is_exact_and_visible() {
        assert_eq!(
            terminal_input_bytes(Some("yes"), true, None).unwrap(),
            b"yes\r"
        );
        assert_eq!(
            terminal_input_display(Some("yes"), true, None).unwrap(),
            "yes ↵"
        );
        assert_eq!(
            terminal_input_bytes(None, false, Some("ctrl-c")).unwrap(),
            b"\x03"
        );
        assert_eq!(
            terminal_input_bytes(None, false, Some("ctrl-l")).unwrap(),
            b"\x0c"
        );
        assert_eq!(
            terminal_input_bytes(None, false, Some("ctrl-z")).unwrap(),
            b"\x1a"
        );
        assert!(terminal_input_bytes(None, false, Some("ctrl-aa")).is_err());
        assert!(terminal_input_bytes(Some("x"), false, Some("enter")).is_err());
    }

    #[test]
    fn noninteractive_process_has_the_fixed_environment() {
        let mut command = Command::new("unused");
        configure_noninteractive_process(&mut command);
        let environment = command
            .get_envs()
            .filter_map(|(name, value)| Some((name.to_str()?, value?.to_str()?)))
            .collect::<std::collections::HashMap<_, _>>();
        for (name, value) in NONINTERACTIVE_ENVIRONMENT {
            assert_eq!(environment.get(name), Some(&value), "missing {name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn noninteractive_process_receives_immediate_stdin_eof() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("if IFS= read -r value; then exit 1; fi");
        configure_noninteractive_process(&mut command);
        assert!(command.status().unwrap().success());
    }

    #[cfg(unix)]
    #[test]
    fn terminal_handle_allows_one_running_session_and_replaces_an_exited_session() {
        let directory = tempfile::tempdir().unwrap();
        let terminal = AgentTerminalHandle::default();
        let running = terminal
            .open_process_for_test(directory.path(), "sleep 5")
            .unwrap();
        assert!(terminal
            .open_process_for_test(directory.path(), "exit 0")
            .is_err());
        assert!(terminal.close_exited(&running).is_err());
        terminal.terminate();
        assert!(terminal.observation(&running).is_err());

        let exited = terminal
            .open_process_for_test(directory.path(), "exit 7")
            .unwrap();
        assert!(terminal.is_running());
        assert!(terminal.monitor_snapshot(INITIAL_COLS).is_some());
        assert!(terminal.poll_monitor_change());
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_running = false;
        loop {
            let observation = terminal.observation(&exited).unwrap();
            if observation["status"] == "exited 7" {
                break;
            }
            saw_running = true;
            assert!(
                Instant::now() < deadline,
                "terminal did not exit: {observation}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            terminal.observation(&exited).unwrap()["status"],
            "exited 7",
            "final screen must remain readable"
        );
        assert!(!terminal.is_running());
        assert!(terminal.monitor_snapshot(INITIAL_COLS).is_none());
        // The exit surfaces as exactly one monitor change, but which call
        // observes it is racy: the poll above may fold in the transition when
        // the process dies fast enough, so require a pending change only when
        // the exit was first seen through an observation, and always require
        // polling to be quiet afterward.
        if saw_running {
            assert!(
                terminal.poll_monitor_change(),
                "the exit must surface as a monitor change"
            );
        } else {
            terminal.poll_monitor_change();
        }
        assert!(!terminal.poll_monitor_change());
        let replacement = terminal
            .open_process_for_test(directory.path(), "exit 0")
            .unwrap();
        assert_ne!(replacement, exited);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let observation = terminal.observation(&replacement).unwrap();
            if observation["status"] == "exited 0" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "replacement terminal did not exit: {observation}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        terminal.close_exited(&replacement).unwrap();
        assert!(terminal.observation(&replacement).is_err());
    }
}
