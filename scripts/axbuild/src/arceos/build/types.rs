use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::build::BuildInfo;

pub type ArceosBuildInfo = BuildInfo;

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct ArceosBuildConfig {
    #[serde(flatten, default)]
    pub(crate) build_info: ArceosBuildInfo,
    #[serde(rename = "app-c", skip_serializing_if = "Option::is_none")]
    pub(crate) app_c: Option<PathBuf>,
}

impl ArceosBuildConfig {
    pub(super) fn default_config() -> Self {
        Self {
            build_info: ArceosBuildInfo::default(),
            app_c: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArceosBuildMode {
    RustStd,
    AppC { app_dir: PathBuf, app_name: String },
}
