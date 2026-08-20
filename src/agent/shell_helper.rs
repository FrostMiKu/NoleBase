//! Hidden Brush runner used by Agent shell tools.

use std::sync::Arc;

use anyhow::{Context, Result};
use brush_builtins::ShellBuilderExt as _;
use brush_core::{ProfileLoadBehavior, RcLoadBehavior, ShellVariable};

const HELPER_FLAG: &str = "--agent-shell-helper";
pub(crate) const NONINTERACTIVE_ENVIRONMENT: [(&str, &str); 7] = [
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("SYSTEMD_PAGER", "cat"),
    ("TERM", "dumb"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("CI", "true"),
    ("NO_COLOR", "1"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShellHelperMode {
    Command(String),
    Interactive,
}

pub(crate) fn shell_helper_mode<I>(mut args: I) -> Option<ShellHelperMode>
where
    I: Iterator<Item = String>,
{
    args.next();
    if args.next().as_deref() != Some(HELPER_FLAG) {
        return None;
    }
    match args.next().as_deref() {
        Some("command") => Some(ShellHelperMode::Command(args.next().unwrap_or_default())),
        Some("interactive") => Some(ShellHelperMode::Interactive),
        _ => None,
    }
}

pub(crate) fn shell_helper_command(mode: &str) -> Result<portable_pty::CommandBuilder> {
    let executable = std::env::current_exe().context("locating the Nole executable")?;
    let mut command = portable_pty::CommandBuilder::new(executable);
    command.arg(HELPER_FLAG);
    command.arg(mode);
    Ok(command)
}

pub(crate) fn run_shell_helper(mode: ShellHelperMode) -> Result<u8> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building Brush runtime")?;
    runtime.block_on(run_shell_helper_async(mode))
}

async fn configured_shell() -> Result<brush_core::Shell> {
    let mut shell = brush_core::Shell::builder()
        .default_builtins(brush_builtins::BuiltinSet::BashMode)
        .interactive(true)
        .login(true)
        .profile(ProfileLoadBehavior::LoadDefault)
        .rc(RcLoadBehavior::LoadDefault)
        .build()
        .await
        .context("loading shell profile")?;

    // Brush follows Bash and loads either login profiles or interactive rc
    // files. Nole deliberately loads both because Agent commands are expected
    // to see the user's aliases and functions as well as their login setup.
    shell.options_mut().login_shell = false;
    shell
        .load_config(&ProfileLoadBehavior::Skip, &RcLoadBehavior::LoadDefault)
        .await
        .context("loading shell rc files")?;
    Ok(shell)
}

fn exported(value: &str) -> ShellVariable {
    let mut variable = ShellVariable::new(value);
    variable.export();
    variable
}

fn apply_noninteractive_environment(shell: &mut brush_core::Shell) -> Result<()> {
    for (name, value) in NONINTERACTIVE_ENVIRONMENT {
        shell
            .set_env_global(name, exported(value))
            .with_context(|| format!("setting {name}"))?;
    }
    Ok(())
}

async fn run_shell_helper_async(mode: ShellHelperMode) -> Result<u8> {
    let mut shell = configured_shell().await?;
    match mode {
        ShellHelperMode::Command(command) => {
            apply_noninteractive_environment(&mut shell)?;
            let result = shell
                .run_string(
                    command,
                    &brush_core::SourceInfo::default(),
                    &shell.default_exec_params(),
                )
                .await
                .context("running shell command")?;
            Ok(u8::from(result.exit_code))
        }
        ShellHelperMode::Interactive => {
            let shell = Arc::new(tokio::sync::Mutex::new(shell));
            let mut input = brush_interactive::BasicInputBackend;
            let options = brush_interactive::InteractiveOptions::default();
            let mut interactive =
                brush_interactive::InteractiveShell::new(&shell, &mut input, &options)?;
            interactive.run_interactively().await?;
            let shell = shell.lock().await;
            let result = shell.last_exit_status();
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_arguments_are_private_and_exact() {
        let args = ["nole", HELPER_FLAG, "command", "printf '%s' hello"]
            .into_iter()
            .map(str::to_string);
        assert_eq!(
            shell_helper_mode(args),
            Some(ShellHelperMode::Command("printf '%s' hello".to_string()))
        );
        let normal = ["nole", "--version"].into_iter().map(str::to_string);
        assert_eq!(shell_helper_mode(normal), None);
    }
}
