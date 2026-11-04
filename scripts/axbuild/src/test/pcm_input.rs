//! Host-side PulseAudio source fixture for PCM capture tests.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, bail, ensure};
use ostool::run::qemu::QemuConfig;
use serde::Deserialize;
use tempfile::TempDir;
use tokio::{
    process::{Child, Command},
    time::{Instant, sleep},
};

use super::{HOST_PCM_AUDIODEV_ID, qemu_has_audiodev};

const PULSE_START_TIMEOUT: Duration = Duration::from_secs(10);
const PULSE_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const PULSE_POLL_INTERVAL: Duration = Duration::from_millis(20);
const PULSE_SOCKET_FILE: &str = "pulse.sock";
const TEST_SOURCE_NAME: &str = "virtio-test";
const TEST_TONE_SINK_NAME: &str = "virtio-test-tone-sink";
const TEST_SINK_NAME: &str = "virtio-test-sink";
const SAMPLE_RATE_HZ: u32 = 48_000;
const MIN_FREQUENCY_HZ: u32 = 1;
const MAX_FREQUENCY_HZ: u32 = 20_000;
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 107;

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    host_pcm_input: Option<InputConfig>,
    #[serde(default)]
    host_pcm_waveform: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputConfig {
    frequency_hz: u32,
}

pub(crate) struct PcmInput {
    pulse: Option<Child>,
    socket: PathBuf,
    _tempdir: TempDir,
}

impl PcmInput {
    /// Starts a private PulseAudio server with one deterministic sine source
    /// and one null playback sink, then points QEMU at both endpoints.
    pub(crate) async fn start(
        config_path: &Path,
        qemu: &mut QemuConfig,
    ) -> anyhow::Result<Option<Self>> {
        let config: Config = toml::from_str(&fs::read_to_string(config_path)?)?;
        let Some(input) = config.host_pcm_input else {
            return Ok(None);
        };
        ensure!(
            config.host_pcm_waveform.is_none(),
            "host_pcm_input and host_pcm_waveform are mutually exclusive"
        );
        ensure!(
            !qemu_has_audiodev(&qemu.args),
            "host PCM input fixture owns -audiodev; do not configure a second backend"
        );
        validate_frequency(input.frequency_hz)?;

        let tempdir = create_private_tempdir()?;

        let socket = tempdir.path().join(PULSE_SOCKET_FILE);
        let socket_text = fixture_socket_text(&socket)?;
        let arguments = pulse_arguments(&socket_text, input.frequency_hz)?;
        let mut command = Command::new("pulseaudio");
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .env("XDG_RUNTIME_DIR", tempdir.path())
            .env("PULSE_RUNTIME_PATH", tempdir.path())
            .kill_on_drop(true);
        let pulse = command
            .spawn()
            .context("failed to start private PulseAudio; expected `pulseaudio` on PATH")?;
        let mut fixture = Self {
            pulse: Some(pulse),
            socket,
            _tempdir: tempdir,
        };
        fixture.wait_for_socket().await?;

        let audiodev = [
            "pa".to_owned(),
            format!("id={HOST_PCM_AUDIODEV_ID}"),
            format!("server={socket_text}"),
            format!("in.name={TEST_SOURCE_NAME}"),
            format!("out.name={TEST_SINK_NAME}"),
        ]
        .join(",");
        qemu.args.extend(["-audiodev".into(), audiodev]);
        Ok(Some(fixture))
    }

    pub(crate) async fn finish(mut self) -> anyhow::Result<()> {
        let mut pulse = self
            .pulse
            .take()
            .context("private PulseAudio child handle is missing")?;
        match pulse.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) => {
                reap_in_background(pulse);
                return Err(error).context("failed to inspect private PulseAudio");
            }
        }
        if let Err(error) = pulse.start_kill() {
            reap_in_background(pulse);
            return Err(error).context("failed to terminate private PulseAudio");
        }
        match tokio::time::timeout(PULSE_STOP_TIMEOUT, pulse.wait()).await {
            Ok(status) => {
                status.context("failed to reap private PulseAudio")?;
            }
            Err(_) => {
                reap_in_background(pulse);
                bail!("timed out waiting for private PulseAudio to exit");
            }
        }
        Ok(())
    }

    async fn wait_for_socket(&mut self) -> anyhow::Result<()> {
        let socket = self.socket.clone();
        let pulse = self
            .pulse
            .as_mut()
            .context("private PulseAudio child handle is missing")?;
        let deadline = Instant::now() + PULSE_START_TIMEOUT;
        loop {
            if let Some(status) = pulse.try_wait()? {
                bail!(
                    "private PulseAudio exited with status {status} before creating `{}`",
                    socket.display()
                );
            }
            match tokio::fs::metadata(&socket).await {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to inspect private PulseAudio socket `{}`",
                            socket.display()
                        )
                    });
                }
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for private PulseAudio socket `{}`",
                socket.display()
            );
            sleep(PULSE_POLL_INTERVAL).await;
        }
        if let Some(status) = pulse.try_wait()? {
            bail!(
                "private PulseAudio exited with status {status} after creating `{}`",
                socket.display()
            );
        }
        Ok(())
    }
}

impl Drop for PcmInput {
    fn drop(&mut self) {
        if let Some(pulse) = self.pulse.take() {
            reap_in_background(pulse);
        }
    }
}

#[cfg(unix)]
fn create_private_tempdir() -> anyhow::Result<TempDir> {
    let tempdir = tempfile::Builder::new()
        .prefix("pcm-input-")
        .tempdir()
        .context("failed to create private PCM input directory")?;
    fs::set_permissions(tempdir.path(), fs::Permissions::from_mode(0o700))
        .context("failed to restrict PCM input directory permissions")?;
    Ok(tempdir)
}

#[cfg(not(unix))]
fn create_private_tempdir() -> anyhow::Result<TempDir> {
    bail!("host PCM input fixture requires a Unix host")
}

fn reap_in_background(mut pulse: Child) {
    let _ = pulse.start_kill();
    // Tokio's ChildDropGuard only signals; owning the handle in a waiter lets
    // the runtime reap it after the fixture future is cancelled.
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let _reaper = runtime.spawn(async move {
            let _ = tokio::time::timeout(PULSE_STOP_TIMEOUT, pulse.wait()).await;
        });
    }
}

fn fixture_socket_text(socket: &Path) -> anyhow::Result<String> {
    let socket = socket
        .to_str()
        .context("private PulseAudio socket path is not UTF-8")?;
    ensure!(
        !socket
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == b',' || byte == 0),
        "private PulseAudio socket path cannot contain whitespace, a comma or NUL"
    );
    ensure!(
        socket.len() <= MAX_UNIX_SOCKET_PATH_BYTES,
        "private PulseAudio socket path exceeds {MAX_UNIX_SOCKET_PATH_BYTES} bytes"
    );
    Ok(socket.to_owned())
}

fn pulse_arguments(socket: &str, frequency_hz: u32) -> anyhow::Result<Vec<String>> {
    validate_frequency(frequency_hz)?;
    // module-sine-source has a fixed two-second latency and no latency modarg.
    // Drive a dedicated null sink instead and remap its monitor source so the
    // capture source can run at normal realtime cadence, independently from
    // the playback sink.
    Ok(vec![
        "-n".to_owned(),
        "--daemonize=no".to_owned(),
        "--exit-idle-time=-1".to_owned(),
        "--use-pid-file=no".to_owned(),
        "--disable-shm".to_owned(),
        format!(
            "--load=module-null-sink sink_name={TEST_SINK_NAME} rate={SAMPLE_RATE_HZ} channels=2"
        ),
        format!(
            "--load=module-null-sink sink_name={TEST_TONE_SINK_NAME} rate={SAMPLE_RATE_HZ} \
             channels=2"
        ),
        format!(
            "--load=module-sine sink={TEST_TONE_SINK_NAME} frequency={frequency_hz} \
             rate={SAMPLE_RATE_HZ}"
        ),
        format!(
            "--load=module-remap-source master={TEST_TONE_SINK_NAME}.monitor \
             source_name={TEST_SOURCE_NAME} rate={SAMPLE_RATE_HZ} channels=2"
        ),
        format!("--load=module-native-protocol-unix socket={socket} auth-anonymous=1"),
    ])
}

fn validate_frequency(frequency_hz: u32) -> anyhow::Result<()> {
    ensure!(
        (MIN_FREQUENCY_HZ..=MAX_FREQUENCY_HZ).contains(&frequency_hz),
        "host PCM input frequency must be in {MIN_FREQUENCY_HZ}..={MAX_FREQUENCY_HZ} Hz, got \
         {frequency_hz}"
    );
    Ok(())
}
