use std::{
    io::{ErrorKind, Read},
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

fn run_case(success_patterns: &[&str], fail_patterns: &[&str]) -> Result<Option<MatchOutcome>> {
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
    let mut tail_bytes = 0usize;

    loop {
        if Instant::now() >= overall_deadline {
            bail!("timed out waiting for matcher outcome");
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

#[test]
fn qemu_byte_stream_success_matches_before_newline() -> Result<()> {
    let Some(outcome) = run_case(
        &[r"Hit any key to stop autoboot:"],
        &[r"__ostool_never_fail__"],
    )?
    else {
        return Ok(());
    };

    assert_eq!(outcome.kind, StreamMatchKind::Success);
    assert_eq!(outcome.matched_regex, r"Hit any key to stop autoboot:");
    assert!(
        outcome
            .matched_text
            .contains("Hit any key to stop autoboot")
    );
    assert!(
        outcome.tail_bytes > 0,
        "expected tail drain bytes after success"
    );
    Ok(())
}

#[test]
fn qemu_byte_stream_fail_matches_before_newline() -> Result<()> {
    let Some(outcome) = run_case(&[r"__ostool_never_success__"], &[r"Net:\s+eth0:"])? else {
        return Ok(());
    };

    assert_eq!(outcome.kind, StreamMatchKind::Fail);
    assert_eq!(outcome.matched_regex, r"Net:\s+eth0:");
    assert!(outcome.matched_text.contains("Net:"));
    assert!(
        outcome.tail_bytes > 0,
        "expected tail drain bytes after fail"
    );
    Ok(())
}

#[test]
fn qemu_byte_stream_fail_wins_when_both_match() -> Result<()> {
    let Some(outcome) = run_case(
        &[r"Hit any key to stop autoboot:"],
        &[r"Hit any key to stop autoboot:"],
    )?
    else {
        return Ok(());
    };

    assert_eq!(outcome.kind, StreamMatchKind::Fail);
    assert_eq!(outcome.matched_regex, r"Hit any key to stop autoboot:");
    assert!(
        outcome
            .matched_text
            .contains("Hit any key to stop autoboot")
    );
    assert!(
        outcome.tail_bytes > 0,
        "expected tail drain bytes after fail"
    );
    Ok(())
}
