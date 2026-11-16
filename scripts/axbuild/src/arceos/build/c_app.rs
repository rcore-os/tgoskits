use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

use super::{ArceosBuildMode, ArceosConfig};
pub(crate) fn arceos_build_mode(config: &ArceosConfig) -> anyhow::Result<ArceosBuildMode> {
    match &config.app_c {
        Some(app_c) => resolve_app_c_mode(app_c),
        None => Ok(ArceosBuildMode::Rust),
    }
}

pub(crate) fn resolve_app_c_mode(app_c: &Path) -> anyhow::Result<ArceosBuildMode> {
    let app_dir = resolve_app_c_dir(app_c)?;
    let app_name = c_app_name(&app_dir)
        .with_context(|| format!("failed to derive C app name from {}", app_dir.display()))?;

    Ok(ArceosBuildMode::AppC { app_dir, app_name })
}

pub(crate) fn resolve_app_c_dir(app_c: &Path) -> anyhow::Result<PathBuf> {
    if !app_c.is_absolute() {
        bail!(
            "app-c source directory {} must be absolute",
            app_c.display()
        );
    }
    if !app_c.is_dir() {
        bail!(
            "app-c source directory {} does not exist or is not a directory",
            app_c.display()
        );
    }
    if !dir_has_direct_c_source(app_c)? {
        bail!(
            "app-c source directory {} must contain at least one direct .c file",
            app_c.display()
        );
    }

    app_c.canonicalize().with_context(|| {
        format!(
            "failed to resolve app-c source directory {}",
            app_c.display()
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
