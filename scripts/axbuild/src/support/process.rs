use std::{
    ffi::OsStr,
    io::{self, Write},
    path::Path,
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result, bail};
use colored::Colorize;

pub trait ProcessExt {
    fn exec(&mut self) -> Result<()>;
    fn exec_capture(&mut self) -> Result<Output>;
}

pub(crate) fn run_cargo_status(workspace_root: &Path, args: &[String]) -> Result<bool> {
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args(args)
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

    fn exec_capture(&mut self) -> Result<Output> {
        print_command(self)?;
        let output = self.output().context("failed to spawn process")?;
        io::stdout()
            .write_all(&output.stdout)
            .context("failed to forward stdout")?;
        io::stderr()
            .write_all(&output.stderr)
            .context("failed to forward stderr")?;

        if output.status.success() {
            Ok(output)
        } else {
            bail!("command exited with status {}", output.status);
        }
    }
}

fn print_command(command: &Command) -> Result<()> {
    let rendered = render_command(command);
    let mut stderr = io::stderr().lock();
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
