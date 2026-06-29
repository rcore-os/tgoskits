use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::build::BuildInfo;

pub type AxvisorBuildInfo = crate::build::BuildInfo;

pub const AXVISOR_PACKAGE: &str = "axvisor";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct AxvisorBoardConfig {
    #[serde(flatten, default)]
    pub(crate) build_info: BuildInfo,
    #[serde(default)]
    pub vm_configs: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct LoadedAxvisorBuildConfig {
    pub(super) build_info: AxvisorBuildInfo,
    pub(super) target: String,
    pub(super) vm_configs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct AxvisorBoardFile {
    pub(crate) target: String,
    #[serde(flatten)]
    pub(crate) config: AxvisorBoardConfig,
}

impl AxvisorBoardFile {
    pub(crate) fn into_board_config(self) -> AxvisorBoardConfig {
        self.config
    }

    pub(super) fn into_loaded(self) -> LoadedAxvisorBuildConfig {
        let Self { target, config } = self;
        config.into_loaded(target)
    }
}

pub(crate) fn default_axvisor_build_info() -> AxvisorBuildInfo {
    let mut build_info = AxvisorBuildInfo::default();
    build_info.features.clear();
    build_info
}

impl AxvisorBoardConfig {
    pub(super) fn into_loaded(self, target: String) -> LoadedAxvisorBuildConfig {
        LoadedAxvisorBuildConfig {
            build_info: self.build_info,
            target,
            vm_configs: self.vm_configs,
        }
    }
}
