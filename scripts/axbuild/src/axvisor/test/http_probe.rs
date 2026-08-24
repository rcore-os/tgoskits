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
//! Exit code 0 is a pass; any nonzero exit fails the case. The asset's output
//! is captured and replayed after QEMU exits so it cannot split UART records.

use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, bail};

use super::types::AxvisorHttpProbeConfig;
use crate::support::process::retry_text_file_busy;

/// Poll interval while waiting for the probe asset to exit.
const PROBE_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

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
    captured_output: Arc<Mutex<Vec<u8>>>,
) -> anyhow::Result<()> {
    let script = case_dir.join(&config.probe_script);
    ensure_probe_asset(&script)?;
    if stop.load(Ordering::Acquire) {
        bail!(
            "probe asset {} was not started because the case had already stopped",
            script.display()
        );
    }
    let mut output = tempfile::tempfile().context("failed to create probe output capture")?;
    let stdout = output
        .try_clone()
        .context("failed to clone probe stdout capture")?;
    let stderr = output
        .try_clone()
        .context("failed to clone probe stderr capture")?;
    let mut child = spawn_probe_asset(
        &script,
        addr,
        config,
        case_dir,
        Stdio::from(stdout),
        Stdio::from(stderr),
    )?;
    let status = wait_probe_asset(&mut child, &stop);
    output
        .seek(SeekFrom::Start(0))
        .context("failed to rewind probe output capture")?;
    let mut captured = format!(
        "  host http probe: running probe asset {}\n",
        script.display()
    )
    .into_bytes();
    output
        .read_to_end(&mut captured)
        .context("failed to read probe output capture")?;
    captured_output.lock().unwrap().extend_from_slice(&captured);

    match status {
        Some(status) if status.success() => Ok(()),
        Some(status) => bail!(
            "probe asset {} exited with code {}",
            script.display(),
            status.code().unwrap_or(-1)
        ),
        None => bail!("probe asset {} was killed", script.display()),
    }
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
/// interpreter; stdout/stderr are captured for ordered replay after QEMU exits.
fn spawn_probe_asset(
    script: &Path,
    addr: &str,
    config: &AxvisorHttpProbeConfig,
    case_dir: &Path,
    stdout: Stdio,
    stderr: Stdio,
) -> anyhow::Result<Child> {
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
        .stdout(stdout)
        .stderr(stderr);
    retry_text_file_busy(|| command.spawn())
        .with_context(|| format!("failed to spawn probe asset {}", script.display()))
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
    #[cfg(unix)]
    use std::{fs, time::Instant};
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };

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

    fn output_capture() -> Arc<Mutex<Vec<u8>>> {
        Arc::new(Mutex::new(Vec::new()))
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

    #[cfg(unix)]
    #[test]
    fn run_executes_the_case_probe_asset_with_env() {
        let dir = fixture_dir();
        let probe = write_fixture_probe(dir.path(), "http_probe.py", 0);
        let config = test_config(PathBuf::from("http_probe.py"));
        let stop = Arc::new(AtomicBool::new(false));

        let result = run(
            "127.0.0.1:12345",
            &config,
            dir.path(),
            stop,
            output_capture(),
        );

        assert!(result.is_ok(), "probe asset should pass: {result:?}");
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
    fn run_propagates_nonzero_probe_exit() {
        let dir = fixture_dir();
        write_fixture_probe(dir.path(), "http_probe.py", 1);
        let config = test_config(PathBuf::from("http_probe.py"));
        let stop = Arc::new(AtomicBool::new(false));

        let error = run(
            "127.0.0.1:12345",
            &config,
            dir.path(),
            stop,
            output_capture(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("exited with code 1"),
            "unexpected probe error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_captures_probe_output_for_deferred_replay() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let dir = fixture_dir();
        let script = dir.path().join("http_probe.py");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'probe stdout\\n'\nprintf 'probe stderr\\n' >&2\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let config = test_config(PathBuf::from("http_probe.py"));
        let stop = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(Mutex::new(Vec::new()));

        run(
            "127.0.0.1:12345",
            &config,
            dir.path(),
            stop,
            captured.clone(),
        )
        .unwrap();

        let captured = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(
            captured.starts_with("  host http probe: running probe asset "),
            "captured output: {captured:?}"
        );
        assert!(
            captured.ends_with("/http_probe.py\nprobe stdout\nprobe stderr\n"),
            "captured output: {captured:?}"
        );
    }

    #[test]
    fn run_rejects_a_missing_probe_asset() {
        let dir = fixture_dir();
        let config = test_config(PathBuf::from("http_probe.py"));
        let stop = Arc::new(AtomicBool::new(false));

        let error = run(
            "127.0.0.1:12345",
            &config,
            dir.path(),
            stop,
            output_capture(),
        )
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
        let probe_thread = thread::spawn(move || {
            run(
                "127.0.0.1:12345",
                &config,
                &case_dir,
                run_stop,
                output_capture(),
            )
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while !started.is_file() {
            assert!(
                Instant::now() < deadline,
                "probe asset did not start before the test deadline"
            );
            thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Release);

        let error = probe_thread.join().unwrap().unwrap_err();
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

        let error = run(
            "127.0.0.1:12345",
            &config,
            dir.path(),
            stop,
            output_capture(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("was not started because the case had already stopped"),
            "unexpected error: {error:#}"
        );
    }

    /// The generic mechanism must execute the actual case asset: the
    /// `http-control-plane` test-suit case carries `http_probe.py` next to its
    /// `qemu-aarch64.toml` and `vm-memory.toml` fixtures. This pins that
    /// contract so a missing/renamed case asset fails this test, not the CI run.
    #[test]
    fn http_control_plane_case_carries_a_probe_asset() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let case_asset = workspace_root.join(
            "test-suit/axvisor/normal/qemu-http-control-plane/http-control-plane/http_probe.py",
        );
        assert!(
            case_asset.is_file(),
            "http-control-plane case missing probe asset: {}",
            case_asset.display()
        );
        // The default `[host_http_probe]` config resolves the asset by name, so
        // the generic runner executes the real case asset unchanged.
        let name = case_asset.file_name().and_then(|s| s.to_str()).unwrap();
        assert_eq!(name, DEFAULT_PROBE_SCRIPT);
    }
}
