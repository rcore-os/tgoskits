use std::path::{Path, PathBuf};

use crate::context::{resolve_workspace_member_dir, workspace_root_path};

pub struct AxvisorContext {
    workspace_root: PathBuf,
    axvisor_dir: PathBuf,
}

impl AxvisorContext {
    pub fn new() -> anyhow::Result<Self> {
        let workspace_root = workspace_root_path()?;
        let axvisor_dir = resolve_workspace_member_dir(crate::axvisor::build::AXVISOR_PACKAGE)?;
        Ok(Self {
            workspace_root,
            axvisor_dir,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn axvisor_dir(&self) -> &Path {
        &self.axvisor_dir
    }
}
