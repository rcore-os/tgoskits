mod c_app;
mod cargo_config;
mod info;
#[cfg(test)]
mod tests;
mod types;

use std::path::{Path, PathBuf};

pub(crate) use c_app::{load_arceos_build_config, load_arceos_build_mode};
pub(crate) use cargo_config::{load_c_app_cargo_config, load_cargo_config};
pub(crate) use info::resolve_build_info_path;
pub use ostool::build::config::LogLevel;
pub(crate) use types::{ArceosBuildConfig, ArceosBuildInfo, ArceosBuildMode};

pub(crate) fn resolve_app_c_dir(config_path: &Path, app_c: &Path) -> anyhow::Result<PathBuf> {
    c_app::resolve_app_c_dir(config_path, app_c)
}

pub(crate) fn resolve_app_c_mode(
    config_path: &Path,
    app_c: &Path,
) -> anyhow::Result<ArceosBuildMode> {
    c_app::resolve_app_c_mode(config_path, app_c)
}

pub(crate) fn default_build_info_path(package: &str, target: &str) -> anyhow::Result<PathBuf> {
    info::default_build_info_path(package, target)
}
