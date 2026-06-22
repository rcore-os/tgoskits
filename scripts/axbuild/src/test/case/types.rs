use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use serde::Deserialize;

pub(super) const CASE_WORK_ROOT_NAME: &str = "qemu-cases";
pub(super) const CASE_CACHE_DIR_NAME: &str = "cache";
pub(super) const CASE_RUNS_DIR_NAME: &str = "runs";
/// Sub-directory under `cache_dir` that holds pre-injected rootfs images.
/// One image per cache key (`{sha256}.img`); present means ready to use.
pub(super) const CASE_ROOTFS_CACHE_DIR_NAME: &str = "rootfs";
pub(super) const CASE_STAGING_DIR_NAME: &str = "staging-root";
pub(super) const CASE_BUILD_DIR_NAME: &str = "build";
pub(super) const CASE_OVERLAY_DIR_NAME: &str = "overlay";
pub(super) const CASE_COMMAND_WRAPPER_DIR_NAME: &str = "guest-bin";
pub(super) const CASE_CROSS_BIN_DIR_NAME: &str = "cross-bin";
pub(super) const CASE_CMAKE_TOOLCHAIN_FILE_NAME: &str = "cmake-toolchain.cmake";
pub(super) const CASE_APK_CACHE_DIR_NAME: &str = "apk-cache";
pub(super) const CASE_SH_DIR_NAME: &str = "sh";
pub(super) const CASE_ROOTFS_COPY_NAME: &str = "case-rootfs.img";
pub(super) const GROUPED_RUNNER_SCRIPT_FORMAT_VERSION: &str =
    "grouped-runner-starry-init-autoload-v1";
pub(super) const PYTHON_PIPELINE_CACHE_VERSION: &str = "python-apk-v1";
pub(super) const RUST_PIPELINE_CACHE_VERSION: &str = "rust-cross-v1";
/// QEMU global snapshot flag -- all disk writes go to a temporary file and are
/// never committed back to the image, keeping the source image pristine.
pub(super) const QEMU_SNAPSHOT_ARG: &str = "-snapshot";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestQemuCase {
    pub(crate) name: String,
    pub(crate) display_name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) qemu_config_path: PathBuf,
    pub(crate) test_commands: Vec<String>,
    pub(crate) host_symbolize_success_regex: Vec<String>,
    pub(crate) host_http_server: Option<HostHttpServerConfig>,
    pub(crate) subcases: Vec<TestQemuSubcase>,
    pub(crate) grouped_subcase_filter: Option<BTreeSet<String>>,
}

impl TestQemuCase {
    pub(crate) fn is_grouped(&self) -> bool {
        !self.test_commands.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct HostHttpServerConfig {
    #[serde(default = "default_host_http_bind")]
    pub(crate) bind: String,
    pub(crate) port: u16,
    #[serde(default = "default_host_http_body")]
    pub(crate) body: String,
    #[serde(default)]
    pub(crate) body_size: Option<usize>,
    #[serde(default = "default_host_http_body_byte")]
    pub(crate) body_byte: u8,
    /// When set, serve files from this host directory (path-routed static file
    /// server with an autoindex at `/`) instead of a fixed body. Lets a guest
    /// drive a real online `pip/uv install --find-links http://10.0.2.2:PORT/`
    /// against a local wheel index over real TCP — hermetic (no internet).
    #[serde(default)]
    pub(crate) dir: Option<String>,
}

fn default_host_http_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_host_http_body() -> String {
    "ArceOS local HTTP fixture\n".to_string()
}

fn default_host_http_body_byte() -> u8 {
    b'a'
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestQemuSubcaseKind {
    C,
    Rust,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestQemuSubcase {
    pub(crate) name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) kind: TestQemuSubcaseKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupedCaseRunnerConfig {
    pub(crate) runner_name: String,
    pub(crate) runner_path: String,
    pub(crate) autorun_profile_script: Option<String>,
    pub(crate) begin_marker: String,
    pub(crate) passed_marker: String,
    pub(crate) failed_marker: String,
    pub(crate) all_passed_marker: String,
    pub(crate) all_failed_marker: String,
    pub(crate) success_regex: String,
    pub(crate) fail_regex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaseScriptEnvConfig {
    pub(crate) staging_root: String,
    pub(crate) case_dir: String,
    pub(crate) case_c_dir: String,
    pub(crate) case_work_dir: String,
    pub(crate) case_build_dir: String,
    pub(crate) case_overlay_dir: String,
}

pub(crate) type GuestPackageEnvPrepareFn =
    fn(&std::path::Path) -> anyhow::Result<Vec<(String, String)>>;

#[derive(Debug, Clone)]
pub(crate) struct CaseAssetConfig {
    pub(crate) grouped_runner: GroupedCaseRunnerConfig,
    pub(crate) script_env: CaseScriptEnvConfig,
    pub(crate) cache_env_vars: Vec<String>,
    pub(crate) prepare_staging_root: fn(&std::path::Path) -> anyhow::Result<()>,
    pub(crate) prepare_guest_package_env: Option<GuestPackageEnvPrepareFn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCaseAssets {
    /// Path of the rootfs image that QEMU should boot from. For cases without
    /// pipeline injection this points directly to the shared source image; for
    /// cases that need injection it points to the per-case temporary copy.
    pub(crate) rootfs_path: PathBuf,
    pub(crate) extra_qemu_args: Vec<String>,
    /// Path of the temporary per-case rootfs copy to remove after the QEMU run,
    /// or `None` when the shared image was used directly (no injection needed).
    pub(crate) rootfs_copy_to_remove: Option<PathBuf>,
    pub(crate) run_dir_to_remove: Option<PathBuf>,
    pub(crate) pipeline: CasePipeline,
    pub(crate) cache_hit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunPreparedQemuCaseOptions {
    pub(crate) prepare_elapsed: Duration,
    pub(crate) qemu_timing_fields: Option<Vec<(&'static str, String)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCaseAssetParts {
    pub(crate) extra_qemu_args: Vec<String>,
    pub(crate) rootfs_path: PathBuf,
    pub(crate) rootfs_copy_to_remove: Option<PathBuf>,
    pub(crate) run_dir_to_remove: Option<PathBuf>,
    pub(crate) pipeline: CasePipeline,
    pub(crate) cache_hit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CasePipeline {
    Plain,
    Grouped,
    C,
    Sh,
    Python,
    Rust,
}

impl CasePipeline {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Grouped => "grouped",
            Self::C => "c",
            Self::Sh => "sh",
            Self::Python => "python",
            Self::Rust => "rust",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaseAssetLayout {
    pub(crate) work_dir: PathBuf,
    pub(crate) run_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    /// Directory holding pre-injected rootfs cache images (`{hash}.img`).
    pub(crate) rootfs_cache_dir: PathBuf,
    pub(crate) staging_root: PathBuf,
    pub(crate) build_dir: PathBuf,
    pub(crate) overlay_dir: PathBuf,
    pub(crate) command_wrapper_dir: PathBuf,
    pub(crate) cross_bin_dir: PathBuf,
    pub(crate) cmake_toolchain_file: PathBuf,
    pub(crate) apk_cache_dir: PathBuf,
    /// Per-case copy of the shared rootfs image, used only when the case needs
    /// pipeline injection (C / shell / Python / grouped). For plain cases no
    /// copy is created and QEMU's `-snapshot` flag keeps the shared image clean.
    pub(crate) case_rootfs_copy: PathBuf,
}
