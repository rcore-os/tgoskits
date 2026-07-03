use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

use super::{ArceosBuildConfig, ArceosBuildFile, ArceosBuildMode};
use crate::build;

pub(crate) fn load_arceos_build_file(path: &Path) -> anyhow::Result<ArceosBuildFile> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read ArceOS build config {}", path.display()))?;
    build::reject_removed_std_field(path, &content)?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse ArceOS build config {}", path.display()))
}

pub(crate) fn load_arceos_build_config(path: &Path) -> anyhow::Result<ArceosBuildConfig> {
    Ok(load_arceos_build_file(path)?.config)
}

pub(crate) fn load_arceos_build_mode(path: &Path) -> anyhow::Result<ArceosBuildMode> {
    let config = load_arceos_build_config(path)?;
    match config.app_c {
        Some(app_c) => super::resolve_app_c_mode(path, &app_c),
        None => Ok(ArceosBuildMode::RustStd),
    }
}

pub(crate) fn resolve_app_c_mode(
    config_path: &Path,
    app_c: &Path,
) -> anyhow::Result<ArceosBuildMode> {
    let app_dir = super::resolve_app_c_dir(config_path, app_c)?;
    let app_name = c_app_name(&app_dir)
        .with_context(|| format!("failed to derive C app name from {}", app_dir.display()))?;

    Ok(ArceosBuildMode::AppC { app_dir, app_name })
}

pub(crate) fn resolve_app_c_dir(config_path: &Path, app_c: &Path) -> anyhow::Result<PathBuf> {
    let app_dir = if app_c.is_absolute() {
        app_c.to_path_buf()
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(app_c)
    };

    if !app_dir.is_dir() {
        bail!(
            "app-c source directory {} configured by {} does not exist or is not a directory",
            app_dir.display(),
            config_path.display()
        );
    }
    if !dir_has_direct_c_source(&app_dir)? {
        bail!(
            "app-c source directory {} configured by {} must contain at least one direct .c file",
            app_dir.display(),
            config_path.display()
        );
    }

    app_dir.canonicalize().with_context(|| {
        format!(
            "failed to resolve app-c source directory {}",
            app_dir.display()
        )
    })
}

fn dir_has_direct_c_source(dir: &Path) -> anyhow::Result<bool> {
    Ok(fs::read_dir(dir)
        .with_context(|| format!("failed to read app-c source directory {}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "c")))
}

fn c_app_name(app_dir: &Path) -> Option<String> {
    let name_dir = if app_dir.file_name().and_then(|name| name.to_str()) == Some("c") {
        app_dir.parent().unwrap_or(app_dir)
    } else {
        app_dir
    };

    name_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
}
