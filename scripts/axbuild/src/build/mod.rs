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
#[cfg(test)]
pub(crate) use platform::workspace_metadata;
use platform::*;
pub(crate) use platform::{
    apply_makefile_features, apply_makefile_features_with_metadata, cached_workspace_metadata,
    default_build_info_path_in_workspace, makefile_features_from_env,
};
pub(crate) use std_build::prepare_std_build_env;
use std_build::*;

#[cfg(test)]
mod tests;
