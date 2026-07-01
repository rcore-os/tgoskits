#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, anyhow, bail};
use ax_config_gen::{GenerateOptions, generate_config, read_config_string};
use cargo_metadata::{Metadata, Package};
use log::{info, warn};
use ostool::build::config::Cargo;
pub use ostool::build::config::LogLevel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::context::{axbuild_tmp_dir, workspace_manifest_path, workspace_metadata_root_manifest};

mod config_file;
mod info;
mod platform;
mod std_build;

pub(crate) use config_file::{
    ensure_build_info, load_build_info, load_toml_with_rejector, read_toml_with_rejector,
    reject_arceos_app_c_field, reject_removed_std_field,
};
pub(crate) use info::{
    ARCEOS_LINKER_SCRIPT, BuildInfo, append_encoded_rustflags, build_info_enables_backtrace_path,
    env_truthy, toolchain_rustflags_for_features,
};
use info::{AXSTD_STD_PACKAGE, PIE_TARGET_DIR, STD_TARGET_DIR, TARGET_JSON_ROOT};
#[cfg(test)]
pub(crate) use platform::parse_makefile_features;
use platform::*;
pub(crate) use platform::{
    ResolvedPlatformConfig, apply_makefile_features, apply_makefile_features_with_metadata,
    cached_workspace_metadata, default_build_info_path_in_workspace, generate_axconfig,
    makefile_features_from_env, resolve_effective_plat_dyn, resolve_platform_config,
    resolve_platform_config_by_package, resolve_platform_config_by_package_with_metadata,
    workspace_metadata,
};
pub(crate) use std_build::prepare_std_build_env;
use std_build::*;

#[cfg(test)]
mod tests;
