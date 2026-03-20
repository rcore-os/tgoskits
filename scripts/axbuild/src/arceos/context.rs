use std::{
    env::current_dir,
    path::{Path, PathBuf},
};

use crate::{
    ArceosConfigOverride,
    arceos::{ArceosConfig, config::resolve_package_app_dir, load_config},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunScope {
    #[default]
    Default,
    PackageRoot,
    StarryOsRoot,
}

pub struct AxContext {
    pub config: ArceosConfig,
    manifest_dir: PathBuf,
    pub package: Option<String>,
    pub qemu_config_path: Option<PathBuf>,
    config_search_dir: PathBuf,
    app_dir: PathBuf,
}

impl AxContext {
    pub fn new(
        overrides: ArceosConfigOverride,
        package: Option<String>,
        qemu_config_path: Option<PathBuf>,
        run_scope: RunScope,
    ) -> anyhow::Result<Self> {
        let manifest_dir = current_dir().unwrap();
        let config = load_config(&manifest_dir, overrides)?;

        let app_dir = resolve_app_dir(&manifest_dir, package.as_deref())?;
        let config_search_dir =
            resolve_config_search_dir(&manifest_dir, package.as_deref(), run_scope)?;

        Ok(Self {
            config,
            package,
            manifest_dir,
            qemu_config_path,
            config_search_dir,
            app_dir,
        })
    }

    pub fn app_dir(&self) -> &Path {
        &self.app_dir
    }

    pub fn manifest_dir(&self) -> &Path {
        &self.manifest_dir
    }

    pub fn config_search_dir(&self) -> &Path {
        &self.config_search_dir
    }
}

fn resolve_app_dir(manifest_dir: &Path, package: Option<&str>) -> anyhow::Result<PathBuf> {
    let Some(package) = package else {
        return Ok(manifest_dir.to_path_buf());
    };

    let app_dir = resolve_package_app_dir(manifest_dir, package)?;
    Ok(manifest_dir.join(app_dir))
}

fn resolve_config_search_dir(
    manifest_dir: &Path,
    package: Option<&str>,
    run_scope: RunScope,
) -> anyhow::Result<PathBuf> {
    match run_scope {
        RunScope::Default => Ok(manifest_dir.to_path_buf()),
        RunScope::PackageRoot => resolve_app_dir(manifest_dir, package),
        RunScope::StarryOsRoot => Ok(manifest_dir.join("os/StarryOS")),
    }
}
