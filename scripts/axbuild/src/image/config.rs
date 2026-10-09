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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StoredImageConfig {
    registry: String,
    download_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extract_dir: Option<PathBuf>,
}

impl ImageConfig {
    pub fn new_default(target_dir: &Path) -> Self {
        Self {
            registry: DEFAULT_REGISTRY_URL.to_string(),
            download_dir: std::env::temp_dir().join("tgosimages"),
            extract_dir: target_dir.join("axbuild").join("rootfs"),
        }
    }

    pub fn get_config_file_path(base_dir: &Path) -> PathBuf {
        crate::context::axbuild_tmp_dir(base_dir).join(IMAGE_CONFIG_FILENAME)
    }

    pub fn read_config(base_dir: &Path, target_dir: &Path) -> anyhow::Result<Self> {
        Self::read_config_with_env(base_dir, target_dir, non_empty_env)
    }

    fn read_config_with_env(
        base_dir: &Path,
        target_dir: &Path,
        env_value: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Self> {
        let path = Self::get_config_file_path(base_dir);
        let default_config = || StoredImageConfig {
            registry: DEFAULT_REGISTRY_URL.to_string(),
            download_dir: std::env::temp_dir().join("tgosimages"),
            extract_dir: None,
        };
        let (stored, original) = match fs::read_to_string(&path) {
            Ok(contents) => {
                let stored = match toml::from_str(&contents) {
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
                (stored, Some(contents))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (default_config(), None),
            Err(error) => {
                return Err(anyhow!(
                    "Failed to read image config file {}: {error}",
                    path.display()
                ));
            }
        };

        let normalized = toml::to_string(&stored)?;
        if original.as_deref() != Some(normalized.as_str()) {
            Self::write_config_contents(&path, &normalized)?;
        }

        let mut config = Self {
            registry: stored.registry,
            download_dir: resolve_path(base_dir, &stored.download_dir),
            extract_dir: stored.extract_dir.map_or_else(
                || target_dir.join("axbuild").join("rootfs"),
                |path| resolve_path(base_dir, &path),
            ),
        };

        if let Some(download_dir) = env_value(DOWNLOAD_DIR_ENV) {
            config.download_dir = resolve_path(base_dir, Path::new(&download_dir));
        }
        if let Some(extract_dir) = env_value(EXTRACT_DIR_ENV) {
            config.extract_dir = resolve_path(base_dir, Path::new(&extract_dir));
        }

        Ok(config)
    }

    pub fn write_config(base_dir: &Path, config: &Self) -> anyhow::Result<()> {
        let path = Self::get_config_file_path(base_dir);
        let stored = StoredImageConfig {
            registry: config.registry.clone(),
            download_dir: config.download_dir.clone(),
            extract_dir: Some(config.extract_dir.clone()),
        };
        Self::write_config_contents(&path, &toml::to_string(&stored)?)
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

fn resolve_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn default_extract_dir_tracks_the_current_target_without_being_persisted() {
        let workspace = tempdir().unwrap();
        let first_target = workspace.path().join("target-one");
        let first =
            ImageConfig::read_config_with_env(workspace.path(), &first_target, |_| None).unwrap();
        assert_eq!(first.extract_dir, first_target.join("axbuild/rootfs"));

        let stored =
            fs::read_to_string(ImageConfig::get_config_file_path(workspace.path())).unwrap();
        assert!(!stored.contains("extract_dir"));

        let second_target = workspace.path().join("target-two");
        let second =
            ImageConfig::read_config_with_env(workspace.path(), &second_target, |_| None).unwrap();
        assert_eq!(second.extract_dir, second_target.join("axbuild/rootfs"));
    }

    #[test]
    fn explicit_config_and_environment_paths_are_workspace_relative() {
        let workspace = tempdir().unwrap();
        ImageConfig::write_config(
            workspace.path(),
            &ImageConfig {
                registry: "https://example.com/registry.toml".to_string(),
                download_dir: PathBuf::from("configured-downloads"),
                extract_dir: PathBuf::from("configured-images"),
            },
        )
        .unwrap();

        let configured = ImageConfig::read_config_with_env(
            workspace.path(),
            &workspace.path().join("target"),
            |_| None,
        )
        .unwrap();
        assert_eq!(
            configured.download_dir,
            workspace.path().join("configured-downloads")
        );
        assert_eq!(
            configured.extract_dir,
            workspace.path().join("configured-images")
        );

        let from_env = ImageConfig::read_config_with_env(
            workspace.path(),
            &workspace.path().join("target"),
            |key| match key {
                DOWNLOAD_DIR_ENV => Some("env-downloads".to_string()),
                EXTRACT_DIR_ENV => Some("env-images".to_string()),
                _ => None,
            },
        )
        .unwrap();
        assert_eq!(
            from_env.download_dir,
            workspace.path().join("env-downloads")
        );
        assert_eq!(from_env.extract_dir, workspace.path().join("env-images"));
    }
}
