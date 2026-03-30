use std::{
    fs,
    path::{Path, PathBuf},
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const DEFAULT_REGISTRY_URL: &str = "https://raw.githubusercontent.com/arceos-hypervisor/axvisor-guest/refs/heads/main/registry/default.toml";
pub const DEFAULT_FALLBACK_REGISTRY_URL: &str = "https://raw.githubusercontent.com/arceos-hypervisor/axvisor-guest/refs/heads/main/registry/v0.0.22.toml";
pub const IMAGE_CONFIG_FILENAME: &str = ".image.toml";
const DEFAULT_AUTO_SYNC_THRESHOLD: u64 = 60 * 60 * 24 * 7;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ImageConfig {
    pub local_storage: PathBuf,
    pub registry: String,
    pub auto_sync: bool,
    pub auto_sync_threshold: u64,
}

impl ImageConfig {
    pub fn new_default() -> Self {
        Self {
            local_storage: std::env::temp_dir().join(".axvisor-images"),
            registry: DEFAULT_REGISTRY_URL.to_string(),
            auto_sync: true,
            auto_sync_threshold: DEFAULT_AUTO_SYNC_THRESHOLD,
        }
    }

    pub fn get_config_file_path(base_dir: &Path) -> PathBuf {
        base_dir.join(IMAGE_CONFIG_FILENAME)
    }

    pub fn read_config(base_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::get_config_file_path(base_dir);

        if !path.exists() {
            Self::write_config(base_dir, &Self::new_default())?;
            return Ok(Self::new_default());
        }

        let s = fs::read_to_string(&path)?;
        toml::from_str(&s).map_err(|e| anyhow!("Invalid image config file: {e}"))
    }

    pub fn write_config(base_dir: &Path, config: &Self) -> anyhow::Result<()> {
        let path = Self::get_config_file_path(base_dir);
        fs::write(path, toml::to_string(config)?)
            .map_err(|e| anyhow!("Failed to write image config file: {e}"))
    }
}

pub(crate) fn fallback_registry_url() -> String {
    std::env::var("AXVISOR_REGISTRY_FALLBACK_URL")
        .unwrap_or_else(|_| DEFAULT_FALLBACK_REGISTRY_URL.to_string())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn read_config_creates_default_when_missing() {
        let dir = tempdir().unwrap();

        let config = ImageConfig::read_config(dir.path()).unwrap();

        assert_eq!(config, ImageConfig::new_default());
        assert!(ImageConfig::get_config_file_path(dir.path()).exists());
    }
}
