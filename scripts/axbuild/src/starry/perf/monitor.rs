use std::{
    fs,
    fs::File,
    io::{BufReader, Read, Write},
    path::Path,
    process::{Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::Context;
use serde::Serialize;

use super::{
    outputs::PerfOutputs,
    qemu::{DEFAULT_STARRY_SHELL_PREFIX, PerfQemuConfig, QemuRun},
};

const SHELL_INIT_WRITE_CHUNK: usize = 64;
const SHELL_INIT_WRITE_DELAY: Duration = Duration::from_millis(1);

#[derive(Default, Serialize)]
pub(super) struct PerfWindowReport {
    pub(super) enabled: bool,
    pub(super) start_marker: Option<String>,
    pub(super) stop_marker: Option<String>,
    pub(super) start_time: Option<f64>,
    pub(super) stop_time: Option<f64>,
    pub(super) duration_sec: Option<f64>,
    pub(super) workload_timeout: Option<u64>,
    pub(super) truncated_by_timeout: bool,
    pub(super) boot_samples_excluded: Option<u64>,
    pub(super) stop_requested: bool,
    pub(super) stop_method: Option<String>,
    pub(super) warnings: Vec<String>,
    pub(super) method: String,
}

pub(super) fn window_report_from_config(config: &PerfQemuConfig) -> PerfWindowReport {
    let enabled = config.start_marker.is_some()
        || config.stop_marker.is_some()
        || config.workload_timeout.is_some();
    let mut report = PerfWindowReport {
        enabled,
        start_marker: config.start_marker.clone(),
        stop_marker: config.stop_marker.clone(),
        workload_timeout: config.workload_timeout,
        method: if enabled {
            "qperf_raw_elapsed_timestamp_filter".to_string()
        } else {
            "disabled".to_string()
        },
        ..PerfWindowReport::default()
    };
    if enabled && config.start_marker.is_none() {
        report
            .warnings
            .push("start marker is not configured; boot samples are not excluded".to_string());
    }
    if config.workload_timeout.is_some() && config.start_marker.is_none() {
        report
            .warnings
            .push("--workload-timeout requires a start marker to open the window".to_string());
    }
    report
}

pub(super) fn run_qemu_with_stdout_monitor(
    mut command: Command,
    config: &PerfQemuConfig,
    outputs: &PerfOutputs,
    overall_timeout: u64,
) -> anyhow::Result<QemuRun> {
    let mut window_report = window_report_from_config(config);
    let shell_init_cmd = config
        .shell_init_cmd
        .as_deref()
        .map(str::trim)
        .filter(|cmd| !cmd.is_empty());
    if shell_init_cmd.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped());
    let mut child = command.spawn().context("failed to spawn QEMU")?;
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take().context("failed to open QEMU stdout")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        let mut buf = [0_u8; 1024];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(len) => {
                    if tx.send(buf[..len].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let started = Instant::now();
    let mut host_stdout = std::io::stdout().lock();
    let mut profile_stdout = File::create(&outputs.profile_stdout)
        .with_context(|| format!("failed to create {}", outputs.profile_stdout.display()))?;
    let mut prompt_window = Vec::new();
    let mut marker_window = Vec::new();
    let mut injected = false;
    let mut echo_disable_deadline = None;
    let shell_prefix = config
        .shell_prefix
        .as_deref()
        .unwrap_or(DEFAULT_STARRY_SHELL_PREFIX);
    let prefix = shell_prefix.as_bytes();
    let start_marker = config.start_marker.as_deref().map(str::as_bytes);
    let stop_marker = config.stop_marker.as_deref().map(str::as_bytes);
    let marker_monitoring = start_marker.is_some() || stop_marker.is_some();

    loop {
        if let Some(status) = child.try_wait().context("failed to poll QEMU")? {
            if shell_init_cmd.is_some() && !injected {
                window_report.warnings.push(format!(
                    "shell prompt `{shell_prefix}` was not observed before QEMU exited"
                ));
                eprintln!(
                    "qperf: shell prompt `{shell_prefix}` was not observed before QEMU exited"
                );
            }
            finalize_window_warnings(&mut window_report);
            return Ok(QemuRun {
                status,
                window: window_report,
            });
        }

        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                profile_stdout
                    .write_all(&chunk)
                    .context("failed to write qperf profile stdout")?;
                host_stdout
                    .write_all(&chunk)
                    .context("failed to forward QEMU stdout")?;
                host_stdout.flush().ok();
                let elapsed = started.elapsed().as_secs_f64();

                if let Some(cmd) = shell_init_cmd
                    && !injected
                    && echo_disable_deadline.is_none()
                {
                    prompt_window.extend_from_slice(&chunk);
                    trim_window(&mut prompt_window, prefix.len().saturating_add(1024));
                    if contains_subslice(&prompt_window, prefix) {
                        let stdin = stdin.as_mut().context("failed to open QEMU stdin")?;
                        if marker_monitoring {
                            stdin
                                .write_all(b"stty -echo 2>/dev/null || true\n")
                                .context("failed to disable shell echo before qperf command")?;
                            stdin.flush().ok();
                            echo_disable_deadline =
                                Some(Instant::now() + Duration::from_millis(150));
                        } else {
                            write_shell_init_command(stdin, cmd)?;
                            injected = true;
                            eprintln!(
                                "qperf: injected shell init command after prompt `{shell_prefix}`"
                            );
                        }
                    }
                }

                if start_marker.is_some() || stop_marker.is_some() {
                    marker_window.extend_from_slice(&chunk);
                    let keep = start_marker
                        .into_iter()
                        .chain(stop_marker)
                        .map(<[u8]>::len)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1024);
                    trim_window(&mut marker_window, keep);
                }

                if window_report.start_time.is_none()
                    && start_marker.is_some_and(|marker| contains_subslice(&marker_window, marker))
                {
                    window_report.start_time = Some(elapsed);
                    eprintln!(
                        "qperf: observed start marker `{}` at {elapsed:.6}s",
                        config.start_marker.as_deref().unwrap_or("")
                    );
                }
                if window_report.stop_time.is_none()
                    && stop_marker.is_some_and(|marker| contains_subslice(&marker_window, marker))
                {
                    window_report.stop_time = Some(elapsed);
                    update_window_duration(&mut window_report);
                    request_qemu_stop(&mut child, outputs, &mut window_report, "stop marker")?;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }

        if let (Some(cmd), Some(deadline)) = (shell_init_cmd, echo_disable_deadline)
            && !injected
            && Instant::now() >= deadline
        {
            let stdin = stdin.as_mut().context("failed to open QEMU stdin")?;
            write_shell_init_command(stdin, cmd)?;
            injected = true;
            echo_disable_deadline = None;
            eprintln!("qperf: injected shell init command after prompt `{shell_prefix}`");
        }

        let elapsed = started.elapsed().as_secs_f64();
        if let (Some(start_time), Some(timeout)) =
            (window_report.start_time, config.workload_timeout)
            && window_report.stop_time.is_none()
            && elapsed - start_time >= timeout as f64
        {
            window_report.stop_time = Some(elapsed);
            window_report.truncated_by_timeout = true;
            update_window_duration(&mut window_report);
            window_report.warnings.push(format!(
                "workload window timed out after {timeout}s without stop marker"
            ));
            request_qemu_stop(&mut child, outputs, &mut window_report, "workload timeout")?;
            break;
        }
        if overall_timeout > 0 && elapsed >= overall_timeout as f64 {
            window_report.warnings.push(format!(
                "QEMU timed out after {overall_timeout}s before workload completed"
            ));
            request_qemu_stop(&mut child, outputs, &mut window_report, "overall timeout")?;
            break;
        }
    }

    let status = wait_for_child_exit(&mut child, Duration::from_secs(20))?;
    if shell_init_cmd.is_some() && !injected {
        window_report.warnings.push(format!(
            "shell prompt `{shell_prefix}` was not observed before QEMU exited"
        ));
        eprintln!("qperf: shell prompt `{shell_prefix}` was not observed before QEMU exited");
    }
    finalize_window_warnings(&mut window_report);
    Ok(QemuRun {
        status,
        window: window_report,
    })
}

fn trim_window(window: &mut Vec<u8>, keep: usize) {
    if window.len() > keep {
        let drain = window.len() - keep;
        window.drain(..drain);
    }
}

fn write_shell_init_command(stdin: &mut impl Write, cmd: &str) -> anyhow::Result<()> {
    for chunk in cmd.as_bytes().chunks(SHELL_INIT_WRITE_CHUNK) {
        stdin
            .write_all(chunk)
            .context("failed to write qperf shell init command")?;
        stdin.flush().ok();
        thread::sleep(SHELL_INIT_WRITE_DELAY);
    }
    stdin
        .write_all(b"\n")
        .context("failed to terminate qperf shell init command")?;
    stdin.flush().ok();
    Ok(())
}

fn update_window_duration(report: &mut PerfWindowReport) {
    report.duration_sec = match (report.start_time, report.stop_time) {
        (Some(start), Some(stop)) if stop >= start => Some(stop - start),
        _ => None,
    };
}

fn request_qemu_stop(
    child: &mut std::process::Child,
    outputs: &PerfOutputs,
    report: &mut PerfWindowReport,
    reason: &str,
) -> anyhow::Result<()> {
    if report.stop_requested {
        return Ok(());
    }
    report.stop_requested = true;
    match request_qmp_quit(&outputs.qmp_socket) {
        Ok(()) => {
            report.stop_method = Some("qmp_quit".to_string());
            eprintln!("qperf: requested QEMU quit via QMP after {reason}");
        }
        Err(err) => {
            report.warnings.push(format!(
                "QMP quit failed after {reason}: {err}; falling back to SIGINT"
            ));
            interrupt_child(child)?;
            report.stop_method = Some("sigint".to_string());
            eprintln!("qperf: sent SIGINT to QEMU after {reason}");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn request_qmp_quit(socket: &Path) -> anyhow::Result<()> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("failed to connect QMP socket {}", socket.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_millis(200)))
        .ok();
    let mut buf = [0_u8; 512];
    let _ = stream.read(&mut buf);
    stream.write_all(b"{\"execute\":\"qmp_capabilities\"}\r\n")?;
    let _ = stream.read(&mut buf);
    stream.write_all(b"{\"execute\":\"quit\"}\r\n")?;
    stream.flush()?;
    Ok(())
}

#[cfg(not(unix))]
fn request_qmp_quit(_socket: &Path) -> anyhow::Result<()> {
    bail!("QMP unix sockets are not supported on this host")
}

#[cfg(unix)]
fn interrupt_child(child: &mut std::process::Child) -> anyhow::Result<()> {
    let pid = child.id() as libc::pid_t;
    if unsafe { libc::kill(pid, libc::SIGINT) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("failed to send SIGINT to QEMU")
    }
}

#[cfg(not(unix))]
fn interrupt_child(child: &mut std::process::Child) -> anyhow::Result<()> {
    child.kill().context("failed to kill QEMU")
}

fn wait_for_child_exit(
    child: &mut std::process::Child,
    timeout: Duration,
) -> anyhow::Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().context("failed to poll QEMU after stop")? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            child.kill().context("failed to kill unresponsive QEMU")?;
            return child.wait().context("failed to wait for killed QEMU");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn finalize_window_warnings(report: &mut PerfWindowReport) {
    if !report.enabled {
        return;
    }
    if report.start_marker.is_some() && report.start_time.is_none() {
        report
            .warnings
            .push("start marker was not observed; folded stacks include boot samples".to_string());
    }
    if report.start_time.is_some() && report.stop_marker.is_some() && report.stop_time.is_none() {
        report
            .warnings
            .push("stop marker was not observed; workload window extends to QEMU exit".to_string());
    }
    update_window_duration(report);
}

pub(super) fn write_window_report(path: &Path, report: &PerfWindowReport) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(report).context("failed to serialize qperf window")?;
    fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
