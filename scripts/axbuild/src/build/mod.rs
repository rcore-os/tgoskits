#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use cargo_metadata::{Metadata, Package};
use log::info;
use ostool::build::config::Cargo;
pub use ostool::build::config::LogLevel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::context::{axbuild_tmp_dir, workspace_manifest_path, workspace_metadata_root_manifest};

mod bare_build;
mod config_file;
mod future_incompat;
mod info;
mod platform;
mod std_build;

pub(crate) use bare_build::{bare_build_target_for, freestanding_build_target_for};
pub(crate) use config_file::{
    ensure_build_info, load_build_info, load_toml_with_rejector, read_toml_with_rejector,
    reject_arceos_app_c_field, reject_removed_std_field,
};
pub(crate) use future_incompat::{
    FutureIncompatReportSession, cargo_target_dir_for, finish_future_incompat_report_session,
    finish_future_incompat_report_status, start_future_incompat_report_session,
};
#[cfg(test)]
pub(crate) use info::toolchain_rustflags_for_features;
pub(crate) use info::{
    ARCEOS_LINKER_SCRIPT, BareKernelLinkMode, BuildInfo, append_cargo_rustflags,
    build_info_enables_backtrace_path, env_truthy, toolchain_rustflags,
};
use info::{PIE_TARGET_DIR, STD_TARGET_DIR, TARGET_JSON_ROOT};
#[cfg(test)]
pub(crate) use platform::parse_makefile_features;
#[cfg(test)]
pub(crate) use platform::workspace_metadata;
use platform::*;
pub(crate) use platform::{
    apply_makefile_features, cached_workspace_metadata, default_build_info_path_in_workspace,
    makefile_features_from_env,
};
use std_build::*;

#[cfg(test)]
mod tests;
