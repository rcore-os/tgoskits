use std::{
    ffi::OsStr,
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use colored::Colorize;

pub trait ProcessExt {
    fn exec(&mut self) -> Result<()>;
    fn exec_quiet(&mut self) -> Result<()>;
}

pub(crate) fn run_cargo_status(workspace_root: &Path, args: &[String]) -> Result<bool> {
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args(args)
        .status()
        .with_context(|| format!("failed to spawn `cargo {}`", args.join(" ")))?;
    Ok(status.success())
}

pub(crate) fn run_cargo_status_with_env(
    workspace_root: &Path,
    args: &[String],
    envs: &[(String, String)],
) -> Result<bool> {
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args(args)
        .envs(envs.iter().map(|(key, value)| (key, value)))
        .status()
        .with_context(|| format!("failed to spawn `cargo {}`", args.join(" ")))?;
    Ok(status.success())
}

impl ProcessExt for Command {
    fn exec(&mut self) -> Result<()> {
        print_command(self)?;
        let status = self
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("failed to spawn process")?;

        if status.success() {
            Ok(())
        } else {
            bail!("command exited with status {status}");
        }
    }

    fn exec_quiet(&mut self) -> Result<()> {
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        exec_quiet_with_writers(self, &mut stdout, &mut stderr)
    }
}

fn print_command(command: &Command) -> Result<()> {
    let mut stderr = io::stderr().lock();
    print_command_to(command, &mut stderr)?;
    Ok(())
}

fn exec_quiet_with_writers(
    command: &mut Command,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let rendered = render_command(command);
    let output = command
        .output()
        .with_context(|| format!("failed to spawn process `{rendered}`"))?;
    if output.status.success() {
        return Ok(());
    }

    print_command_to(command, stderr)?;
    stdout
        .write_all(&output.stdout)
        .context("failed to replay command stdout")?;
    stderr
        .write_all(&output.stderr)
        .context("failed to replay command stderr")?;
    bail!("command exited with status {}", output.status);
}

fn print_command_to(command: &Command, stderr: &mut dyn Write) -> Result<()> {
    let rendered = render_command(command);
    writeln!(stderr, "{}", rendered.purple()).context("failed to print command")?;
    Ok(())
}

fn render_command(command: &Command) -> String {
    let mut parts = Vec::new();

    if let Some(dir) = command.get_current_dir() {
        parts.push(format!("cd {} &&", shell_escape(dir.as_os_str())));
    }

    parts.push(shell_escape(command.get_program()));
    parts.extend(command.get_args().map(shell_escape));
    parts.join(" ")
}

fn shell_escape(value: &OsStr) -> String {
    let value = value.to_string_lossy();
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '_' | '-' | '.' | '=' | ':'))
    {
        value.into_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    #[test]
    fn quiet_command_discards_success_output() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf 'successful stdout'; printf 'successful stderr' >&2",
        ]);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        super::exec_quiet_with_writers(&mut command, &mut stdout, &mut stderr).unwrap();

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    #[test]
    fn quiet_command_replays_failed_output_and_command() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf 'failed stdout'; printf 'failed stderr' >&2; exit 7",
        ]);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let error =
            super::exec_quiet_with_writers(&mut command, &mut stdout, &mut stderr).unwrap_err();

        assert_eq!(stdout, b"failed stdout");
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("sh -c"));
        assert!(stderr.contains("failed stderr"));
        assert!(error.to_string().contains("exit status: 7"));
    }
}
