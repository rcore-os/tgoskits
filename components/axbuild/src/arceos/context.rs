use std::{
    env::current_dir,
    path::{Path, PathBuf},
};

use anyhow::Context;
use cargo_metadata::MetadataCommand;

use crate::{
    ArceosConfig, ArceosConfigOverride, arceos::config::resolve_package_app_dir, load_config,
};

pub struct AxContext {
    pub config: ArceosConfig,
    manifest_dir: PathBuf,
    pub package: Option<String>,
    pub qemu_config_path: Option<PathBuf>,
    app_dir: PathBuf,
}

impl AxContext {
    pub fn new(
        overrides: ArceosConfigOverride,
        package: Option<String>,
        qemu_config_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let manifest_dir = current_dir().unwrap();
        let config = load_config(&manifest_dir, overrides)?;

        let app_dir = resolve_app_dir(&manifest_dir, package.as_deref())?;

        Ok(Self {
            config,
            package,
            manifest_dir,
            qemu_config_path,
            app_dir,
        })
    }

    pub fn app_dir(&self) -> &Path {
        &self.app_dir
    }

    pub fn manifest_dir(&self) -> &Path {
        &self.manifest_dir
    }
}

fn resolve_app_dir(manifest_dir: &Path, package: Option<&str>) -> anyhow::Result<PathBuf> {
    let Some(package) = package else {
        return Ok(manifest_dir.to_path_buf());
    };

    let app_dir = resolve_package_app_dir(manifest_dir, package)?;
    Ok(manifest_dir.join(app_dir))
}
