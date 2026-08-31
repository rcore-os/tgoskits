use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/rcore-os/tgosimages/refs/heads/main/registry/default.toml";
pub const IMAGE_CONFIG_FILENAME: &str = ".image.toml";
const DOWNLOAD_DIR_ENV: &str = "TGOS_IMAGE_DOWNLOAD_DIR";
const EXTRACT_DIR_ENV: &str = "TGOS_IMAGE_EXTRACT_DIR";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ImageConfig {
    pub registry: String,
    pub download_dir: PathBuf,
    pub extract_dir: PathBuf,
}

impl ImageConfig {
    pub fn new_default(base_dir: &Path) -> Self {
        let axbuild_tmp_dir = crate::context::axbuild_tmp_dir(base_dir);
        Self {
            registry: DEFAULT_REGISTRY_URL.to_string(),
            download_dir: std::env::temp_dir().join("tgosimages"),
            extract_dir: axbuild_tmp_dir.join("rootfs"),
        }
    }

    pub fn get_config_file_path(base_dir: &Path) -> PathBuf {
        crate::context::axbuild_tmp_dir(base_dir).join(IMAGE_CONFIG_FILENAME)
    }

    pub fn read_config(base_dir: &Path) -> anyhow::Result<Self> {
        Self::read_config_with_env(base_dir, non_empty_env)
    }

    fn read_config_with_env(
        base_dir: &Path,
        env_value: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Self> {
        let path = Self::get_config_file_path(base_dir);
        let default_config = || Self::new_default(base_dir);
        let (mut config, original) = match fs::read_to_string(&path) {
            Ok(contents) => {
                let config = match toml::from_str(&contents) {
                    Ok(config) => config,
                    Err(error) => {
                        eprintln!(
                            "image config at {} does not match the current format; regenerating \
                             defaults: {error}",
                            path.display()
                        );
                        default_config()
                    }
                };
                (config, Some(contents))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (default_config(), None),
            Err(error) => {
                return Err(anyhow!(
                    "Failed to read image config file {}: {error}",
                    path.display()
                ));
            }
        };

        let normalized = toml::to_string(&config)?;
        if original.as_deref() != Some(normalized.as_str()) {
            Self::write_config_contents(&path, &normalized)?;
        }

        if let Some(download_dir) = env_value(DOWNLOAD_DIR_ENV) {
            config.download_dir = PathBuf::from(download_dir);
        }
        if let Some(extract_dir) = env_value(EXTRACT_DIR_ENV) {
            config.extract_dir = PathBuf::from(extract_dir);
        }

        Ok(config)
    }

    pub fn write_config(base_dir: &Path, config: &Self) -> anyhow::Result<()> {
        let path = Self::get_config_file_path(base_dir);
        Self::write_config_contents(&path, &toml::to_string(config)?)
    }

    fn write_config_contents(path: &Path, contents: &str) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| anyhow!("Failed to create image config directory: {e}"))?;
        }
        fs::write(path, contents).map_err(|e| anyhow!("Failed to write image config file: {e}"))
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}
