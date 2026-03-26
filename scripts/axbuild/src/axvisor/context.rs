use std::path::{Path, PathBuf};

use crate::{
    axvisor::image::{config::IMAGE_CONFIG_FILENAME, storage::REGISTRY_FILENAME},
    context::workspace_root_path,
};

pub struct AxvisorContext {
    workspace_root: PathBuf,
}

impl AxvisorContext {
    pub fn new() -> anyhow::Result<Self> {
        let workspace_root = workspace_root_path()?;
        Ok(Self { workspace_root })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn image_config_path(&self) -> PathBuf {
        self.workspace_root.join(IMAGE_CONFIG_FILENAME)
    }

    pub fn registry_file_path(&self, local_storage: &Path) -> PathBuf {
        local_storage.join(REGISTRY_FILENAME)
    }
}
