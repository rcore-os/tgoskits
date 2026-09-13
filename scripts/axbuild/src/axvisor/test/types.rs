use std::path::{Path, PathBuf};

use ostool::run::qemu::QemuConfig;
use serde::Deserialize;

use crate::test::{board as board_test, case::TestQemuCase, qemu as test_qemu};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AxvisorQemuCase {
    pub(crate) case: TestQemuCase,
    pub(crate) build_group: String,
    pub(crate) build_config_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreparedAxvisorQemuCase {
    pub(super) case: AxvisorQemuCase,
    pub(super) qemu: QemuConfig,
}

impl test_qemu::BuildConfigRef for PreparedAxvisorQemuCase {
    fn build_group(&self) -> &str {
        &self.case.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.case.build_config_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) build_config: PathBuf,
    pub(crate) board_test_config_path: PathBuf,
}

impl board_test::BoardTestGroupInfo for BoardTestGroup {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_name(&self) -> &str {
        &self.board_name
    }
}

/// Host-side probe configuration for the AxVisor management HTTP control plane.
///
/// Direction is the reverse of the generic [`HostHttpServerConfig`](crate::test::case::HostHttpServerConfig):
/// instead of the host serving fixtures to the guest, the host acts as a
/// *client* that probes the axum management API running *inside* the guest,
/// over QEMU user-mode networking hostfwd
/// (`-netdev user,hostfwd=tcp::<host_port>-:<guest_port>`). The *test content*
/// — the concrete requests, fixtures, and assertions — lives with the test-suit
/// case as an executable probe asset (see [`probe_script`](Self::probe_script));
/// the generic runner only orchestrates: forward the port, execute the asset,
/// collect its exit code, and report the result. This config is
/// AxVisor-specific: it carries the bearer token, timeouts, and probe-asset
/// name the runner passes on, so it lives in the AxVisor test layer rather than
/// the generic test layer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct AxvisorHttpProbeConfig {
    /// Guest-side port the in-guest HTTP server binds to. The harness forwards a
    /// freshly picked host port to it via hostfwd, so the two never collide.
    #[serde(default = "default_probe_guest_port")]
    pub(crate) guest_port: u16,
    /// Total seconds the probe may spend retrying the initial TCP connect before
    /// giving up (guest boot + network init). Must be less than the QEMU case
    /// `timeout` so a broken server fails on the probe, not on the QEMU timeout.
    /// Passed to the probe asset as `AXVISOR_HTTP_CONNECT_TIMEOUT`.
    #[serde(default = "default_probe_connect_timeout_secs")]
    pub(crate) connect_timeout_secs: u64,
    /// Per-request HTTP timeout so a hung in-guest server fails a single request
    /// fast and the probe asset's poll loops can retry instead of blocking the
    /// runner thread forever. Passed to the probe asset as
    /// `AXVISOR_HTTP_REQUEST_TIMEOUT`.
    #[serde(default = "default_probe_request_timeout_secs")]
    pub(crate) request_timeout_secs: u64,
    /// Executable probe asset, resolved against the case directory. It owns all
    /// concrete requests/assertions for the case, so new HTTP scenarios or
    /// API-contract changes edit the case asset rather than this crate. The
    /// runner spawns it once the forwarded port is reachable and treats the
    /// exit code as the verdict (0 = pass). Defaults to `http_probe.py`.
    #[serde(default = "default_probe_script")]
    pub(crate) probe_script: PathBuf,
    /// Bearer token the probe must send on authenticated requests, matching the
    /// guest build's `[env] AXVM_HTTP_TOKEN`. The probe also asserts that an
    /// *unauthenticated* write request is rejected with 401 (the access-denied
    /// regression the management-control-plane security review requires).
    /// Passed to the probe asset as `AXVISOR_HTTP_TOKEN`.
    #[serde(default)]
    pub(crate) token: Option<String>,
}

/// Default probe-asset file name inside the case directory.
pub(crate) const DEFAULT_PROBE_SCRIPT: &str = "http_probe.py";

fn default_probe_guest_port() -> u16 {
    8080
}

fn default_probe_script() -> PathBuf {
    PathBuf::from(DEFAULT_PROBE_SCRIPT)
}

fn default_probe_connect_timeout_secs() -> u64 {
    120
}

fn default_probe_request_timeout_secs() -> u64 {
    5
}
