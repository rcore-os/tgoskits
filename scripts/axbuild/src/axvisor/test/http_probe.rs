//! Generic host-side probe runner for the AxVisor management HTTP control plane.
//!
//! Direction is host -> guest: the probe dials the axum management API running
//! *inside* the AxVisor guest through QEMU user-mode networking hostfwd, and
//! asserts the responses entirely host-side. The *test content* — the concrete
//! requests, fixtures, and assertions — lives with the test-suit case as an
//! executable probe asset (default `http_probe.py` in the case directory; see
//! [`AxvisorHttpProbeConfig::probe_script`](super::types::AxvisorHttpProbeConfig::probe_script)).
//! New HTTP scenarios or API-contract changes therefore edit the case asset,
//! never this crate.
//!
//! This module is generic orchestration only: resolve the probe asset, spawn it
//! once the forwarded port is reachable, and collect its exit code as the
//! verdict. The runner's
//! [`HostHttpProbeGuard`](super::host_probe::HostHttpProbeGuard) does the rest
//! of the orchestration: wait for the forwarded port, invoke this probe, store
//! its verdict, and quit QEMU over QMP.
//!
//! The probe asset is executed directly (its shebang selects the interpreter)
//! with the environment:
//!
//! ```text
//! AXVISOR_HTTP_BASE            http://127.0.0.1:<host_port> (forwarded)
//! AXVISOR_HTTP_TOKEN           bearer token (may be empty)
//! AXVISOR_HTTP_CASE_DIR        case directory (fixtures like `vm-memory.toml`)
//! AXVISOR_HTTP_CONNECT_TIMEOUT seconds for the initial reachability wait
//! AXVISOR_HTTP_REQUEST_TIMEOUT seconds per HTTP request
//! ```
//!
//! Exit code 0 is a pass; any nonzero exit fails the case. The asset's combined
//! stdout/stderr is captured for replay after QEMU exits, so its diagnostics do
//! not interleave with the live serial stream.

use std::{
    io::{self, Read},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, bail};

use super::{host_probe::HostHttpProbeOutcome, types::AxvisorHttpProbeConfig};
use crate::support::process::retry_text_file_busy;

/// Poll interval while waiting for the probe asset to exit.
const PROBE_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_PROBE_OUTPUT_BYTES: usize = 1024 * 1024;

/// Run the case's HTTP probe asset against one boot.
///
/// `addr` is the forwarded host address (`127.0.0.1:<port>`). `config` carries
/// the bearer token, timeouts, and the probe-asset name; `case_dir` locates
/// the asset (and its fixtures). `stop` is the shared abort flag: when the
/// runner marks the case over (QEMU failure, timeout), a still-running asset is
/// killed instead of waiting it out.
pub(crate) fn run(
    addr: &str,
    config: &AxvisorHttpProbeConfig,
    case_dir: &Path,
    stop: Arc<AtomicBool>,
) -> HostHttpProbeOutcome {
    let script = case_dir.join(&config.probe_script);
    let captured = (|| -> anyhow::Result<(Vec<u8>, Option<ExitStatus>)> {
        ensure_probe_asset(&script)?;
        if stop.load(Ordering::Acquire) {
            bail!(
                "probe asset {} was not started because the case had already stopped",
                script.display()
            );
        }
        let (mut child, output_capture) = spawn_probe_asset(&script, addr, config, case_dir)?;
        let status = wait_probe_asset(&mut child, &stop);
        let bytes = output_capture.finish()?;
        Ok((bytes, status))
    })();

    match captured {
        Ok((output, Some(status))) if status.success() => HostHttpProbeOutcome {
            output,
            verdict: Ok(()),
        },
        Ok((output, Some(status))) => HostHttpProbeOutcome {
            output,
            verdict: Err(anyhow::anyhow!(
                "probe asset {} exited with code {}",
                script.display(),
                status.code().unwrap_or(-1)
            )),
        },
        Ok((output, None)) => HostHttpProbeOutcome {
            output,
            verdict: Err(anyhow::anyhow!(
                "probe asset {} was killed",
                script.display()
            )),
        },
        Err(error) => HostHttpProbeOutcome::failed(error),
    }
}

#[derive(Default)]
struct BoundedProbeOutput {
    bytes: Vec<u8>,
    dropped_bytes: usize,
}

impl BoundedProbeOutput {
    fn append(&mut self, chunk: &[u8]) {
        let remaining = MAX_PROBE_OUTPUT_BYTES.saturating_sub(self.bytes.len());
        let keep = remaining.min(chunk.len());
        self.bytes.extend_from_slice(&chunk[..keep]);
        self.dropped_bytes = self
            .dropped_bytes
            .saturating_add(chunk.len().saturating_sub(keep));
    }

    fn finish(mut self) -> Vec<u8> {
        if self.dropped_bytes != 0 {
            use std::io::Write as _;
            let _ = writeln!(
                self.bytes,
                "\n[probe output truncated: dropped {} bytes]",
                self.dropped_bytes
            );
        }
        self.bytes
    }
}

struct ProbeOutputCapture {
    reader: JoinHandle<io::Result<BoundedProbeOutput>>,
}

impl ProbeOutputCapture {
    fn start(reader: os_pipe::PipeReader) -> Self {
        Self {
            reader: spawn_output_reader(reader),
        }
    }

    fn finish(self) -> anyhow::Result<Vec<u8>> {
        let output = self
            .reader
            .join()
            .map_err(|_| anyhow::anyhow!("probe output reader thread panicked"))?
            .context("failed to drain probe output pipe")?;
        Ok(output.finish())
    }
}

fn spawn_output_reader(
    mut reader: impl Read + Send + 'static,
) -> JoinHandle<io::Result<BoundedProbeOutput>> {
    thread::spawn(move || {
        let mut output = BoundedProbeOutput::default();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Ok(output);
            }
            output.append(&buffer[..read]);
        }
    })
}

/// Fail fast when the configured probe asset is missing, so a case that
/// references a nonexistent asset errors clearly instead of spawning a `not
/// found` and misreporting it as a probe failure.
fn ensure_probe_asset(script: &Path) -> anyhow::Result<()> {
    if !script.is_file() {
        bail!(
            "probe asset {} does not exist; add it to the case directory (or set \
             [host_http_probe] probe_script)",
            script.display()
        );
    }
    Ok(())
}

/// Spawn the probe asset with the forwarded base URL, token, and timeouts as
/// environment. The asset is executed directly so its shebang picks the
/// interpreter; stdout/stderr share one pipe drained by a bounded reader so a
/// noisy probe cannot grow temporary storage or block on a full pipe.
fn spawn_probe_asset(
    script: &Path,
    addr: &str,
    config: &AxvisorHttpProbeConfig,
    case_dir: &Path,
) -> anyhow::Result<(Child, ProbeOutputCapture)> {
    let (reader, stdout) = os_pipe::pipe().context("failed to create probe output pipe")?;
    let stderr = stdout
        .try_clone()
        .context("failed to clone probe output pipe")?;
    let mut command = Command::new(script);
    command
        .env("AXVISOR_HTTP_BASE", format!("http://{addr}"))
        .env(
            "AXVISOR_HTTP_TOKEN",
            config.token.clone().unwrap_or_default(),
        )
        .env("AXVISOR_HTTP_CASE_DIR", case_dir)
        .env(
            "AXVISOR_HTTP_CONNECT_TIMEOUT",
            config.connect_timeout_secs.to_string(),
        )
        .env(
            "AXVISOR_HTTP_REQUEST_TIMEOUT",
            config.request_timeout_secs.to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let child = retry_text_file_busy(|| command.spawn())
        .with_context(|| format!("failed to spawn probe asset {}", script.display()))?;
    Ok((child, ProbeOutputCapture::start(reader)))
}

/// Wait for the probe asset to exit, killing it if the runner marks the case
/// over. Returns `Some(status)` on a normal exit and `None` if it was killed.
fn wait_probe_asset(child: &mut Child, stop: &AtomicBool) -> Option<ExitStatus> {
    loop {
        if stop.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        if let Some(status) = child.try_wait().ok().flatten() {
            return Some(status);
        }
        thread::sleep(PROBE_EXIT_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::{fs, time::Instant};

    use serde::Deserialize;

    use super::{super::types::DEFAULT_PROBE_SCRIPT, *};

    fn fixture_dir() -> tempfile::TempDir {
        #[cfg(unix)]
        {
            // Some CI runners mount their system temporary directory with
            // `noexec`. Probe fixtures are deliberately executable, so keep
            // them below the workspace build directory instead.
            let root = std::env::current_dir()
                .unwrap()
                .join("target")
                .join("axbuild-http-probe-fixtures");
            fs::create_dir_all(&root).unwrap();
            tempfile::Builder::new()
                .prefix("probe-")
                .tempdir_in(root)
                .unwrap()
        }

        #[cfg(not(unix))]
        {
            tempfile::tempdir().unwrap()
        }
    }

    fn test_config(probe_script: PathBuf) -> AxvisorHttpProbeConfig {
        AxvisorHttpProbeConfig {
            guest_port: 8080,
            connect_timeout_secs: 120,
            request_timeout_secs: 5,
            probe_script,
            token: Some("t".into()),
        }
    }

    /// Parse a `[host_http_probe]` section like
    /// [`load_axvisor_http_probe_config`](super::super::qemu::load_axvisor_http_probe_config).
    fn parse_probe_section(toml_body: &str) -> AxvisorHttpProbeConfig {
        #[derive(Deserialize)]
        struct ProbeSection {
            #[serde(default)]
            host_http_probe: Option<AxvisorHttpProbeConfig>,
        }
        toml::from_str::<ProbeSection>(toml_body)
            .expect("probe section parses")
            .host_http_probe
            .expect("host_http_probe present")
    }

    /// Write an executable probe asset that records its environment and exits
    /// with `code`.
    #[cfg(unix)]
    fn write_fixture_probe(dir: &Path, name: &str, code: i32) -> PathBuf {
        use std::{fs, os::unix::fs::PermissionsExt};
        let path = dir.join(name);
        let script = format!(
            "#!/bin/sh\nprintf '%s' \
             \"$AXVISOR_HTTP_BASE|$AXVISOR_HTTP_TOKEN|$AXVISOR_HTTP_CASE_DIR\" > \
             \"$AXVISOR_HTTP_CASE_DIR/env.txt\"\nexit {code}\n"
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Write an executable probe asset that records startup and then blocks.
    #[cfg(unix)]
    fn write_blocking_fixture_probe(dir: &Path, name: &str) -> PathBuf {
        use std::{fs, os::unix::fs::PermissionsExt};
        let path = dir.join(name);
        let script =
            "#!/bin/sh\nprintf started > \"$AXVISOR_HTTP_CASE_DIR/started.txt\"\nexec sleep 30\n";
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn write_output_fixture_probe(dir: &Path, name: &str) -> PathBuf {
        use std::{fs, os::unix::fs::PermissionsExt};
        let path = dir.join(name);
        fs::write(
            &path,
            "#!/bin/sh\nprintf 'probe stdout\\n'\nprintf 'probe stderr\\n' >&2\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(target_os = "linux")]
    fn write_large_output_fixture_probe(dir: &Path, name: &str) -> PathBuf {
        use std::{fs, os::unix::fs::PermissionsExt};
        let path = dir.join(name);
        fs::write(
            &path,
            format!(
                "#!/usr/bin/env python3\nimport os, \
                 sys\nprint(os.readlink('/proc/self/fd/1'))\nsys.stdout.write('x' * {})\n",
                MAX_PROBE_OUTPUT_BYTES + 64 * 1024
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn probe_script_defaults_to_http_probe_py() {
        let config = parse_probe_section("[host_http_probe]\ntoken = \"t\"\n");
        assert_eq!(config.probe_script, PathBuf::from(DEFAULT_PROBE_SCRIPT));
    }

    #[test]
    fn probe_script_is_configurable() {
        let config = parse_probe_section("[host_http_probe]\nprobe_script = \"custom_probe.sh\"\n");
        assert_eq!(config.probe_script, PathBuf::from("custom_probe.sh"));
    }

    #[test]
    fn probe_asset_spawn_uses_the_text_file_busy_retry_boundary() {
        let source = include_str!("http_probe.rs");
        let retry_spawn = ["retry_text_file_busy", "(|| command.spawn())"].concat();

        assert!(
            source.contains(&retry_spawn),
            "directly executed probe assets must retry the transient ETXTBSY publication window"
        );
    }

    #[test]
    fn captured_probe_output_is_bounded_without_changing_execution_verdict() {
        let mut output = BoundedProbeOutput::default();
        output.append(&vec![b'x'; MAX_PROBE_OUTPUT_BYTES + 1]);
        let output = output.finish();

        assert!(output.starts_with(&vec![b'x'; MAX_PROBE_OUTPUT_BYTES]));
        assert!(output.ends_with(b"\n[probe output truncated: dropped 1 bytes]\n"));
    }

    #[cfg(unix)]
    #[test]
    fn run_executes_the_case_probe_asset_with_env() {
        let dir = fixture_dir();
        let probe = write_fixture_probe(dir.path(), "http_probe.py", 0);
        let config = test_config(PathBuf::from("http_probe.py"));
        let stop = Arc::new(AtomicBool::new(false));

        let outcome = run("127.0.0.1:12345", &config, dir.path(), stop);

        assert!(
            outcome.verdict.is_ok(),
            "probe asset should pass: {:?}",
            outcome.verdict
        );
        // The generic mechanism really executed the asset and forwarded the
        // env the asset needs to dial the guest API.
        let recorded = std::fs::read_to_string(dir.path().join("env.txt")).unwrap();
        assert_eq!(
            recorded,
            "http://127.0.0.1:12345|t|".to_string() + &dir.path().to_string_lossy()
        );
        assert!(probe.exists());
    }

    #[cfg(unix)]
    #[test]
    fn run_captures_probe_output_for_deferred_replay() {
        let dir = fixture_dir();
        write_output_fixture_probe(dir.path(), "http_probe.py");
        let config = test_config(PathBuf::from("http_probe.py"));
        let stop = Arc::new(AtomicBool::new(false));

        let outcome = run("127.0.0.1:12345", &config, dir.path(), stop);

        assert!(outcome.verdict.is_ok());
        assert_eq!(
            String::from_utf8(outcome.output).unwrap(),
            "probe stdout\nprobe stderr\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn run_bounds_a_real_probe_while_it_writes_to_a_pipe() {
        let dir = fixture_dir();
        write_large_output_fixture_probe(dir.path(), "http_probe.py");
        let config = test_config(PathBuf::from("http_probe.py"));
        let stop = Arc::new(AtomicBool::new(false));

        let outcome = run("127.0.0.1:12345", &config, dir.path(), stop);

        assert!(outcome.verdict.is_ok());
        assert!(outcome.output.starts_with(b"pipe:["));
        assert!(
            outcome
                .output
                .windows(b"[probe output truncated: dropped ".len())
                .any(|window| window == b"[probe output truncated: dropped ")
        );
        assert!(outcome.output.len() <= MAX_PROBE_OUTPUT_BYTES + 80);
    }

    #[cfg(unix)]
    #[test]
    fn run_propagates_nonzero_probe_exit() {
        let dir = fixture_dir();
        write_fixture_probe(dir.path(), "http_probe.py", 1);
        let config = test_config(PathBuf::from("http_probe.py"));
        let stop = Arc::new(AtomicBool::new(false));

        let error = run("127.0.0.1:12345", &config, dir.path(), stop)
            .verdict
            .unwrap_err();
        assert!(
            error.to_string().contains("exited with code 1"),
            "unexpected probe error: {error:#}"
        );
    }

    #[test]
    fn run_rejects_a_missing_probe_asset() {
        let dir = fixture_dir();
        let config = test_config(PathBuf::from("http_probe.py"));
        let stop = Arc::new(AtomicBool::new(false));

        let error = run("127.0.0.1:12345", &config, dir.path(), stop)
            .verdict
            .unwrap_err();
        assert!(error.to_string().contains("does not exist"));
    }

    #[cfg(unix)]
    #[test]
    fn run_kills_the_probe_asset_when_stop_is_requested() {
        let dir = fixture_dir();
        write_blocking_fixture_probe(dir.path(), "http_probe.py");
        let config = test_config(PathBuf::from("http_probe.py"));
        let case_dir = dir.path().to_path_buf();
        let started = case_dir.join("started.txt");
        let stop = Arc::new(AtomicBool::new(false));
        let run_stop = stop.clone();
        let probe_thread =
            thread::spawn(move || run("127.0.0.1:12345", &config, &case_dir, run_stop));

        let deadline = Instant::now() + Duration::from_secs(5);
        while !started.is_file() {
            assert!(
                Instant::now() < deadline,
                "probe asset did not start before the test deadline"
            );
            thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Release);

        let error = probe_thread.join().unwrap().verdict.unwrap_err();
        assert!(
            error.to_string().contains("was killed"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_does_not_spawn_probe_when_stop_was_already_requested() {
        let dir = fixture_dir();
        let probe = dir.path().join("http_probe.py");
        std::fs::write(&probe, "#!/bin/sh\nexit 0\n").unwrap();
        let config = test_config(PathBuf::from("http_probe.py"));
        let stop = Arc::new(AtomicBool::new(true));

        let error = run("127.0.0.1:12345", &config, dir.path(), stop)
            .verdict
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("was not started because the case had already stopped"),
            "unexpected error: {error:#}"
        );
    }
}
