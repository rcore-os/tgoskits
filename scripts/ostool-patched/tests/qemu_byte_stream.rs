//! QEMU-backed byte-stream matcher integration tests.

use std::{
    io::{ErrorKind, Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use ostool::run::{ByteStreamMatcher, StreamMatchKind};
use regex::Regex;

static PORT: AtomicU32 = AtomicU32::new(11000);
const SUCCESS_MARKER: &str = "__OSTOOL_QEMU_SUCCESS_MARKER__";
const FAIL_MARKER: &str = "__OSTOOL_QEMU_FAIL_MARKER__";
const BOTH_MARKER: &str = "__OSTOOL_QEMU_BOTH_MARKER__";
const NEVER_MATCH_REGEX: &str = r"__ostool_never_match__";
const MARKER_COMMAND_DELAY: Duration = Duration::from_secs(2);
const MARKER_COMMAND_INTERVAL: Duration = Duration::from_millis(300);

struct QemuGuard(Option<Child>);

impl QemuGuard {
    fn shutdown(mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for QemuGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn qemu_binary() -> &'static str {
    "qemu-system-aarch64"
}

fn uboot_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/u-boot.bin")
}

/// Starts QEMU with U-Boot and returns a TCP serial stream.
fn spawn_uboot_qemu() -> Result<(QemuGuard, TcpStream)> {
    let port = PORT.fetch_add(1, Ordering::SeqCst);
    let bin = uboot_bin();

    let child = Command::new(qemu_binary())
        .arg("-serial")
        .arg(format!("tcp::{port},server,nowait"))
        .args([
            "-machine",
            "virt",
            "-cpu",
            "cortex-a57",
            "-nographic",
            // Avoid QEMU's default NIC path, which can require efi-virtio.rom
            // before the TCP serial endpoint becomes available in slim images.
            "-netdev",
            "user,id=net0",
            "-device",
            "virtio-net-device,netdev=net0",
            "-bios",
            bin.to_str()
                .context("u-boot.bin path contains invalid UTF-8")?,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            if err.kind() == ErrorKind::NotFound {
                anyhow::anyhow!("qemu-system-aarch64 is not installed")
            } else {
                err.into()
            }
        })?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port as u16)) {
            stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .context("failed to set read timeout")?;
            return Ok((QemuGuard(Some(child)), stream));
        }

        if Instant::now() >= deadline {
            return Err(anyhow::anyhow!(
                "timed out waiting for QEMU serial port on {port}"
            ));
        }

        thread::sleep(Duration::from_millis(100));
    }
}

struct MatchOutcome {
    kind: StreamMatchKind,
    matched_regex: String,
    matched_text: String,
    tail_bytes: usize,
}

fn marker_regex(marker: &str) -> String {
    format!(r"(?m)^{}", regex::escape(marker))
}

fn drive_uboot_marker_command(
    stream: &mut TcpStream,
    started_at: Instant,
    last_write: &mut Option<Instant>,
    marker: &str,
) -> Result<()> {
    let now = Instant::now();
    if last_write
        .as_ref()
        .is_some_and(|last| now.duration_since(*last) < MARKER_COMMAND_INTERVAL)
    {
        return Ok(());
    }

    if now.duration_since(started_at) < MARKER_COMMAND_DELAY {
        stream
            .write_all(b"\x03\r")
            .context("failed to interrupt U-Boot autoboot")?;
    } else {
        write!(stream, "echo {marker}\r").context("failed to write U-Boot marker command")?;
    }

    *last_write = Some(now);
    Ok(())
}

fn run_case(
    success_patterns: &[&str],
    fail_patterns: &[&str],
    marker: &str,
) -> Result<Option<MatchOutcome>> {
    let (guard, mut stream) = match spawn_uboot_qemu() {
        Ok(pair) => pair,
        Err(err) if err.to_string().contains("not installed") => {
            eprintln!("skipping qemu-backed test: {err}");
            return Ok(None);
        }
        Err(err) => return Err(err),
    };

    let success_regex: Vec<Regex> = success_patterns
        .iter()
        .map(|p| Regex::new(p).with_context(|| format!("invalid success regex: {p}")))
        .collect::<Result<_, _>>()?;
    let fail_regex: Vec<Regex> = fail_patterns
        .iter()
        .map(|p| Regex::new(p).with_context(|| format!("invalid fail regex: {p}")))
        .collect::<Result<_, _>>()?;

    let mut matcher = ByteStreamMatcher::new(success_regex, fail_regex);
    let mut buffer = [0u8; 512];
    let overall_deadline = Instant::now() + Duration::from_secs(15);
    let command_started_at = Instant::now();
    let mut last_command_write = None;
    let mut tail_bytes = 0usize;

    loop {
        if Instant::now() >= overall_deadline {
            bail!("timed out waiting for matcher outcome");
        }

        if matcher.matched().is_none() {
            drive_uboot_marker_command(
                &mut stream,
                command_started_at,
                &mut last_command_write,
                marker,
            )?;
        }

        let timeout = if matcher.matched().is_some() {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(100)
        };
        stream
            .set_read_timeout(Some(timeout))
            .context("failed to update read timeout")?;

        match stream.read(&mut buffer) {
            Ok(0) => {
                if let Some(matched) = matcher.matched() {
                    guard.shutdown();
                    return Ok(Some(MatchOutcome {
                        kind: matched.kind,
                        matched_regex: matched.matched_regex.clone(),
                        matched_text: matched.matched_text.clone(),
                        tail_bytes,
                    }));
                }
            }
            Ok(n) => {
                for &byte in &buffer[..n] {
                    let was_matched = matcher.matched().is_some();
                    matcher.observe_byte(byte);
                    if was_matched {
                        tail_bytes += 1;
                    }
                }

                if matcher.should_stop() {
                    guard.shutdown();
                    let matched = matcher.matched().unwrap();
                    return Ok(Some(MatchOutcome {
                        kind: matched.kind,
                        matched_regex: matched.matched_regex.clone(),
                        matched_text: matched.matched_text.clone(),
                        tail_bytes,
                    }));
                }
            }
            Err(err)
                if err.kind() == ErrorKind::TimedOut || err.kind() == ErrorKind::WouldBlock =>
            {
                if matcher.should_stop() {
                    guard.shutdown();
                    let matched = matcher.matched().unwrap();
                    return Ok(Some(MatchOutcome {
                        kind: matched.kind,
                        matched_regex: matched.matched_regex.clone(),
                        matched_text: matched.matched_text.clone(),
                        tail_bytes,
                    }));
                }
            }
            Err(err) => return Err(err.into()),
        }
    }
}

/// Verifies a success regex can match before the newline is drained.
#[test]
fn qemu_byte_stream_success_matches_before_newline() -> Result<()> {
    // Drive U-Boot to print a marker line controlled by the test fixture. The
    // line-anchor keeps the matcher from accepting the echoed command itself.
    let success_marker_regex = marker_regex(SUCCESS_MARKER);
    let Some(outcome) = run_case(
        &[success_marker_regex.as_str()],
        &[NEVER_MATCH_REGEX],
        SUCCESS_MARKER,
    )?
    else {
        return Ok(());
    };

    assert_eq!(outcome.kind, StreamMatchKind::Success);
    assert_eq!(outcome.matched_regex, success_marker_regex);
    assert!(outcome.matched_text.contains(SUCCESS_MARKER));
    assert!(
        outcome.tail_bytes > 0,
        "expected tail drain bytes after success"
    );
    Ok(())
}

/// Verifies a fail regex can match before the newline is drained.
#[test]
fn qemu_byte_stream_fail_matches_before_newline() -> Result<()> {
    let fail_marker_regex = marker_regex(FAIL_MARKER);
    let Some(outcome) = run_case(
        &[NEVER_MATCH_REGEX],
        &[fail_marker_regex.as_str()],
        FAIL_MARKER,
    )?
    else {
        return Ok(());
    };

    assert_eq!(outcome.kind, StreamMatchKind::Fail);
    assert_eq!(outcome.matched_regex, fail_marker_regex);
    assert!(outcome.matched_text.contains(FAIL_MARKER));
    assert!(
        outcome.tail_bytes > 0,
        "expected tail drain bytes after fail"
    );
    Ok(())
}

/// Verifies fail matches take precedence when both regex sets match.
#[test]
fn qemu_byte_stream_fail_wins_when_both_match() -> Result<()> {
    let both_marker_regex = marker_regex(BOTH_MARKER);
    let Some(outcome) = run_case(
        &[both_marker_regex.as_str()],
        &[both_marker_regex.as_str()],
        BOTH_MARKER,
    )?
    else {
        return Ok(());
    };

    assert_eq!(outcome.kind, StreamMatchKind::Fail);
    assert_eq!(outcome.matched_regex, both_marker_regex);
    assert!(outcome.matched_text.contains(BOTH_MARKER));
    assert!(
        outcome.tail_bytes > 0,
        "expected tail drain bytes after fail"
    );
    Ok(())
}
