use std::path::{Path, PathBuf};

use crate::{
    axvisor::image::{config::IMAGE_CONFIG_FILENAME, storage::REGISTRY_FILENAME},
    context::{workspace_member_dir, workspace_root_path},
};

pub struct AxvisorContext {
    workspace_root: PathBuf,
    axvisor_dir: PathBuf,
}

impl AxvisorContext {
    pub fn new() -> anyhow::Result<Self> {
        let workspace_root = workspace_root_path()?;
        let axvisor_dir = workspace_member_dir(crate::axvisor::build::AXVISOR_PACKAGE)?;
        Ok(Self {
            workspace_root,
            axvisor_dir,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_in(workspace_root: PathBuf, axvisor_dir: PathBuf) -> Self {
        Self {
            workspace_root,
            axvisor_dir,
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn axvisor_dir(&self) -> &Path {
        &self.axvisor_dir
    }

    pub fn image_config_path(&self) -> PathBuf {
        self.workspace_root.join(IMAGE_CONFIG_FILENAME)
    }

    pub fn registry_file_path(&self, local_storage: &Path) -> PathBuf {
        local_storage.join(REGISTRY_FILENAME)
    }
}
