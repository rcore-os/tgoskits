use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use colored::Colorize;

const TEXT_FILE_BUSY_RETRY_LIMIT: usize = 5;
const TEXT_FILE_BUSY_RETRY_DELAY: Duration = Duration::from_millis(10);

pub trait ProcessExt {
    /// Encodes an option and value as one argv element for parsers that would
    /// otherwise interpret a hyphen-prefixed value as another option.
    fn arg_option_value(&mut self, option: &str, value: &OsStr) -> &mut Self;
    fn exec(&mut self) -> Result<()>;
    fn exec_quiet(&mut self) -> Result<()>;
}

/// Retry the Linux executable-publication race without changing other process
/// errors. Callers should use this only when the program may have just been
/// materialized or replaced.
pub(crate) fn retry_text_file_busy<T>(
    mut operation: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut retries_remaining = TEXT_FILE_BUSY_RETRY_LIMIT;
    loop {
        match operation() {
            Err(err) if is_text_file_busy(&err) && retries_remaining > 0 => {
                retries_remaining -= 1;
                std::thread::sleep(TEXT_FILE_BUSY_RETRY_DELAY);
            }
            result => return result,
        }
    }
}

#[cfg(unix)]
fn is_text_file_busy(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ETXTBSY)
}

#[cfg(not(unix))]
fn is_text_file_busy(_error: &io::Error) -> bool {
    false
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

pub(crate) fn find_host_binary_candidates(candidates: &[&str]) -> Result<std::path::PathBuf> {
    candidates
        .iter()
        .find_map(|candidate| find_optional_host_binary(candidate))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "required host binary was not found in PATH; tried: {}",
                candidates.join(", ")
            )
        })
}

fn find_optional_host_binary(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

impl ProcessExt for Command {
    fn arg_option_value(&mut self, option: &str, value: &OsStr) -> &mut Self {
        let mut argument = OsString::from(option);
        argument.push("=");
        argument.push(value);
        self.arg(argument)
    }

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
    use std::{ffi::OsStr, process::Command};

    use super::ProcessExt;

    #[cfg(unix)]
    #[test]
    fn retries_transient_text_file_busy_errors() {
        let mut attempts = 0;
        let result = super::retry_text_file_busy(|| {
            attempts += 1;
            if attempts <= super::TEXT_FILE_BUSY_RETRY_LIMIT {
                Err(std::io::Error::from_raw_os_error(libc::ETXTBSY))
            } else {
                Ok("spawned")
            }
        });

        assert_eq!(result.unwrap(), "spawned");
        assert_eq!(attempts, super::TEXT_FILE_BUSY_RETRY_LIMIT + 1);
    }

    #[cfg(unix)]
    #[test]
    fn does_not_retry_other_spawn_errors() {
        let mut attempts = 0;
        let error = super::retry_text_file_busy(|| {
            attempts += 1;
            Err::<(), _>(std::io::Error::from_raw_os_error(libc::ENOENT))
        })
        .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn option_values_are_one_argument_even_when_the_value_starts_with_a_hyphen() {
        let mut command = Command::new("python3");

        command
            .arg_option_value("--qemu-arg", OsStr::new("-cpu"))
            .arg_option_value("--shell-init-cmd", OsStr::new("--version"));

        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, ["--qemu-arg=-cpu", "--shell-init-cmd=--version"]);
    }

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
