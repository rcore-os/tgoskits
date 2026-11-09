#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use ax_config::RuntimeValues;
pub use ax_config::{CommonConfig, LogLevel};
use cargo_metadata::{Metadata, Package};
use ostool::build::config::Cargo;
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::context::axbuild_tmp_dir;
#[cfg(test)]
use crate::context::{workspace_manifest_path, workspace_metadata_root_manifest};

mod bare_build;
mod config_file;
mod config_source;
mod future_incompat;
mod info;
mod platform;
mod std_build;
pub(crate) mod symbol_map;

pub(crate) use bare_build::{bare_build_target_for, freestanding_build_target_for};
pub(crate) use config_file::{
    BuildConfiguration, invalid_config_value, reject_extra_fields, validate_config_target,
    validate_resource_path,
};
#[cfg(test)]
pub(crate) use config_file::{load_build_config, parse_build_config};
pub use config_source::ConfigSource;
pub(crate) use config_source::{
    ConfigInput, FileFields, export_config, load_config_input, show_config,
};
pub(crate) use future_incompat::{
    FutureIncompatReportSession, finish_future_incompat_report_session,
    finish_future_incompat_report_status, start_future_incompat_report_session,
};
pub(crate) use info::{
    ARCEOS_LINKER_SCRIPT, BareKernelLinkMode, append_cargo_rustflags, env_truthy,
    into_prepared_no_std_cargo_config_with_metadata,
    into_prepared_no_std_cargo_config_with_metadata_and_defaults,
    into_prepared_std_cargo_config_with_metadata,
    into_prepared_std_cargo_config_with_metadata_and_defaults, package_enables_mmu,
    resolve_c_app_features, toolchain_rustflags, validate_feature, validate_features,
};
use info::{PIE_TARGET_DIR, STD_TARGET_DIR, TARGET_JSON_ROOT};
#[cfg(test)]
pub(crate) use info::{into_base_cargo_config, into_prepared_base_cargo_config_with_metadata};
#[cfg(test)]
pub(crate) use platform::workspace_metadata;
use platform::*;
pub(crate) use platform::{
    apply_makefile_features, default_build_info_path_in_workspace, makefile_features_from_env,
};
pub(crate) use std_build::std_check_target_for;
use std_build::*;

#[cfg(test)]
mod tests;
