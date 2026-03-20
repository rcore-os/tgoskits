// Copyright 2025 The tgoskits Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{
    ffi::OsStr,
    io::{self, Read, Write},
    path::Path,
    process::{Command, ExitStatus, Stdio},
    thread,
};

use anyhow::{Context, Result, anyhow};
use colored::Colorize;

#[derive(Debug)]
pub(crate) struct CapturedOutput {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

pub(crate) fn run_status(command: &mut Command, desc: impl AsRef<str>) -> Result<()> {
    let desc = desc.as_ref();
    print_command(command);
    let status = command
        .status()
        .with_context(|| format!("Failed to run {desc}"))?;
    ensure_success(status, desc)
}

pub(crate) fn run_output(command: &mut Command, desc: impl AsRef<str>) -> Result<CapturedOutput> {
    let desc = desc.as_ref();
    prepare_output_command(command);
    print_command(command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to run {desc}"))?;

    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("Failed to capture stdout for {desc}"))?;
    let stderr = child
        .stderr
        .take()
        .with_context(|| format!("Failed to capture stderr for {desc}"))?;

    let stdout_handle = spawn_forwarder(stdout, Stream::Stdout);
    let stderr_handle = spawn_forwarder(stderr, Stream::Stderr);

    let status = child
        .wait()
        .with_context(|| format!("Failed to wait for {desc}"))?;
    let stdout = join_forwarder(stdout_handle, desc, "stdout")?;
    let stderr = join_forwarder(stderr_handle, desc, "stderr")?;

    ensure_success(status, desc)?;
    Ok(CapturedOutput { stdout, stderr })
}

fn ensure_success(status: ExitStatus, desc: &str) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("{desc} failed with status: {status}"))
    }
}

fn spawn_forwarder<R>(reader: R, stream: Stream) -> thread::JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || forward_and_capture(reader, stream))
}

fn join_forwarder(
    handle: thread::JoinHandle<io::Result<Vec<u8>>>,
    desc: &str,
    stream_name: &str,
) -> Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| anyhow!("Output forwarder thread for {desc} panicked"))?
        .with_context(|| format!("Failed to forward {stream_name} for {desc}"))
}

fn forward_and_capture<R>(mut reader: R, stream: Stream) -> io::Result<Vec<u8>>
where
    R: Read,
{
    let mut captured = Vec::new();
    let mut buf = [0u8; 8192];

    match stream {
        Stream::Stdout => {
            let stdout = io::stdout();
            let mut writer = stdout.lock();
            loop {
                let read = reader.read(&mut buf)?;
                if read == 0 {
                    writer.flush()?;
                    return Ok(captured);
                }
                writer.write_all(&buf[..read])?;
                writer.flush()?;
                captured.extend_from_slice(&buf[..read]);
            }
        }
        Stream::Stderr => {
            let stderr = io::stderr();
            let mut writer = stderr.lock();
            loop {
                let read = reader.read(&mut buf)?;
                if read == 0 {
                    writer.flush()?;
                    return Ok(captured);
                }
                writer.write_all(&buf[..read])?;
                writer.flush()?;
                captured.extend_from_slice(&buf[..read]);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

fn prepare_output_command(command: &mut Command) {
    command.env_remove("NO_COLOR");
    maybe_set_env(command, "CLICOLOR_FORCE", "1");
    if matches!(command_name(command).as_deref(), Some("cargo")) {
        maybe_set_env(command, "CARGO_TERM_COLOR", "always");
    }
}

fn maybe_set_env(command: &mut Command, key: &str, value: &str) {
    if !command
        .get_envs()
        .any(|(existing_key, _)| existing_key == OsStr::new(key))
    {
        command.env(key, value);
    }
}

fn print_command(command: &Command) {
    eprintln!(
        "{}",
        format!("Running: {}", format_command(command)).purple()
    );
}

fn format_command(command: &Command) -> String {
    let mut parts = Vec::new();
    parts.push(format_arg(command.get_program()));
    parts.extend(command.get_args().map(format_arg));
    parts.join(" ")
}

fn format_arg(arg: &OsStr) -> String {
    let rendered = arg.to_string_lossy();
    if rendered.is_empty()
        || rendered
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'')
    {
        format!("{rendered:?}")
    } else {
        rendered.into_owned()
    }
}

fn command_name(command: &Command) -> Option<String> {
    Path::new(command.get_program())
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{command_name, format_command, prepare_output_command, run_output, run_status};

    #[cfg(unix)]
    fn shell_command(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    #[cfg(unix)]
    #[test]
    fn run_output_captures_stdout_and_stderr() {
        let mut command = shell_command("printf 'out'; printf 'err' >&2");
        let output = run_output(&mut command, "test command").expect("command should succeed");

        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }

    #[cfg(unix)]
    #[test]
    fn run_status_reports_non_zero_exit() {
        let mut command = shell_command("exit 7");
        let err = run_status(&mut command, "failing command").expect_err("command should fail");

        assert!(
            err.to_string()
                .contains("failing command failed with status")
        );
    }

    #[test]
    fn format_command_quotes_whitespace_arguments() {
        let mut command = Command::new("cargo");
        command.arg("axplat").arg("info").arg("hello world");

        assert_eq!(
            format_command(&command),
            r#"cargo axplat info "hello world""#
        );
    }

    #[test]
    fn prepare_output_command_forces_color_for_cargo() {
        let mut command = Command::new("cargo");
        prepare_output_command(&mut command);

        let envs = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(command_name(&command).as_deref(), Some("cargo"));
        assert!(
            envs.iter()
                .any(|(key, value)| key == "CLICOLOR_FORCE" && value.as_deref() == Some("1"))
        );
        assert!(
            envs.iter()
                .any(|(key, value)| key == "CARGO_TERM_COLOR" && value.as_deref() == Some("always"))
        );
    }
}
