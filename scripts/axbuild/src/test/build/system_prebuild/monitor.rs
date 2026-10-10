use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, Command},
    sync::mpsc,
    thread,
    time::Instant,
};

use anyhow::Context;

use super::{
    PREBUILD_FAILED_MARKER, PREBUILD_PASSED_MARKER, PREBUILD_SHELL_PROMPT, PREBUILD_TIMEOUT,
};

pub(super) fn monitor_prebuild_process(
    mut command: Command,
    program: &Path,
    injection: Option<&str>,
    forward_output: bool,
) -> anyhow::Result<()> {
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {}", program.display()))?;
    let outcome = (|| -> anyhow::Result<()> {
        let stdout = child
            .stdout
            .take()
            .context("failed to capture QEMU stdout")?;
        let mut stdin = child.stdin.take();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut stdout = stdout;
            let mut buffer = [0_u8; 1024];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(len) if tx.send(buffer[..len].to_vec()).is_err() => break,
                    Ok(_) => {}
                }
            }
        });

        let deadline = Instant::now() + PREBUILD_TIMEOUT;
        let mut marker_window = Vec::new();
        let mut prompt_window = Vec::new();
        let mut injected = injection.is_none();
        let mut host_stdout = std::io::stdout().lock();
        loop {
            if let Some(status) = child.try_wait().context("failed to poll prebuild QEMU")? {
                break Err(anyhow::anyhow!(
                    "{} exited with {status} before prebuild completed",
                    program.display()
                ));
            }
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(chunk) => {
                    if forward_output {
                        host_stdout.write_all(&chunk)?;
                        host_stdout.flush().ok();
                    }
                    marker_window.extend_from_slice(&chunk);
                    trim_marker_window(&mut marker_window);
                    if !injected {
                        prompt_window.extend_from_slice(&chunk);
                        let keep = PREBUILD_SHELL_PROMPT.len() + 1024;
                        if prompt_window.len() > keep {
                            prompt_window.drain(..prompt_window.len() - keep);
                        }
                        if contains_bytes(&prompt_window, PREBUILD_SHELL_PROMPT) {
                            let stdin = stdin
                                .as_mut()
                                .context("failed to open prebuild QEMU stdin")?;
                            stdin.write_all(injection.unwrap_or_default().as_bytes())?;
                            stdin.write_all(b"\n")?;
                            stdin.flush().ok();
                            injected = true;
                        }
                    }
                    if contains_bytes(&marker_window, PREBUILD_PASSED_MARKER.as_bytes()) {
                        break Ok(());
                    }
                    if contains_bytes(&marker_window, PREBUILD_FAILED_MARKER.as_bytes()) {
                        break Err(anyhow::anyhow!(
                            "prebuild.sh failed inside {}",
                            program.display()
                        ));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(anyhow::anyhow!(
                        "{} output closed before prebuild completed",
                        program.display()
                    ));
                }
            }
            if Instant::now() >= deadline {
                break Err(anyhow::anyhow!(
                    "prebuild.sh timed out after {}s inside {}",
                    PREBUILD_TIMEOUT.as_secs(),
                    program.display()
                ));
            }
        }
    })();

    let cleanup = cleanup_prebuild_child(&mut child);
    outcome.and(cleanup)
}

fn cleanup_prebuild_child(child: &mut Child) -> anyhow::Result<()> {
    if child
        .try_wait()
        .context("failed to poll prebuild QEMU during cleanup")?
        .is_none()
        && let Err(error) = child.kill()
        && child
            .try_wait()
            .context("failed to poll prebuild QEMU after cleanup error")?
            .is_none()
    {
        return Err(error).context("failed to stop prebuild QEMU");
    }
    child
        .wait()
        .context("failed to wait for prebuild QEMU during cleanup")?;
    Ok(())
}

fn trim_marker_window(window: &mut Vec<u8>) {
    let keep = PREBUILD_PASSED_MARKER
        .len()
        .max(PREBUILD_FAILED_MARKER.len())
        + 1024;
    if window.len() > keep {
        window.drain(..window.len() - keep);
    }
}

fn contains_bytes(output: &[u8], marker: &[u8]) -> bool {
    output
        .windows(marker.len())
        .any(|candidate| candidate == marker)
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    use super::*;

    #[test]
    fn marker_matching_survives_chunk_trimming() {
        let mut output = vec![b'x'; 4096];
        output.extend_from_slice(PREBUILD_PASSED_MARKER.as_bytes());
        trim_marker_window(&mut output);
        assert!(contains_bytes(&output, PREBUILD_PASSED_MARKER.as_bytes()));
        assert!(!contains_bytes(&output, PREBUILD_FAILED_MARKER.as_bytes()));
    }

    #[test]
    fn process_monitor_accepts_success_and_propagates_failure_markers() {
        let mut success = Command::new("sh");
        success.args([
            "-c",
            &format!("printf '%s' {PREBUILD_PASSED_MARKER}; sleep 10"),
        ]);
        success.stdout(Stdio::piped()).stderr(Stdio::null());
        let started = Instant::now();
        monitor_prebuild_process(success, Path::new("successful-prebuild"), None, false).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));

        let mut failure = Command::new("sh");
        failure.args([
            "-c",
            &format!("printf '%s' {PREBUILD_FAILED_MARKER}; sleep 10"),
        ]);
        failure.stdout(Stdio::piped()).stderr(Stdio::null());
        let error = monitor_prebuild_process(failure, Path::new("failed-prebuild"), None, false)
            .unwrap_err();
        assert!(error.to_string().contains("prebuild.sh failed"));
    }

    #[test]
    fn cleanup_stops_running_child() {
        let mut child = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        cleanup_prebuild_child(&mut child).unwrap();
        assert!(child.try_wait().unwrap().is_some());
    }
}
