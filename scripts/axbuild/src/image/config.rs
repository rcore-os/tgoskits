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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn read_config_creates_default_when_missing() {
        let dir = tempdir().unwrap();

        let config = ImageConfig::read_config_with_env(dir.path(), |_| None).unwrap();

        assert_eq!(config, ImageConfig::new_default(dir.path()));
        assert_eq!(config.download_dir, std::env::temp_dir().join("tgosimages"));
        assert_eq!(config.extract_dir, dir.path().join("tmp/axbuild/rootfs"));
        assert_eq!(
            ImageConfig::get_config_file_path(dir.path()),
            dir.path().join("tmp/axbuild/.image.toml")
        );
        assert!(ImageConfig::get_config_file_path(dir.path()).exists());
    }

    #[test]
    fn read_config_applies_directory_env_overrides() {
        let dir = tempdir().unwrap();
        let download_dir = dir.path().join("persistent-downloads");
        let extract_dir = dir.path().join("working-rootfs");

        let config = ImageConfig::read_config_with_env(dir.path(), |key| match key {
            DOWNLOAD_DIR_ENV => Some(download_dir.display().to_string()),
            EXTRACT_DIR_ENV => Some(extract_dir.display().to_string()),
            _ => None,
        })
        .unwrap();

        assert_eq!(config.download_dir, download_dir);
        assert_eq!(config.extract_dir, extract_dir);
    }

    #[test]
    fn read_config_accepts_separate_download_and_extract_dirs() {
        let dir = tempdir().unwrap();
        let config_path = ImageConfig::get_config_file_path(dir.path());
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            config_path,
            format!(
                r#"
registry = "https://example.com/registry.toml"
download_dir = "{}"
extract_dir = "{}"
"#,
                dir.path().join("downloads").display(),
                dir.path().join("rootfs").display()
            ),
        )
        .unwrap();

        let config = ImageConfig::read_config_with_env(dir.path(), |_| None).unwrap();

        assert_eq!(config.download_dir, dir.path().join("downloads"));
        assert_eq!(config.extract_dir, dir.path().join("rootfs"));
    }

    #[test]
    fn read_config_recreates_old_format_without_migrating_removed_fields() {
        let dir = tempdir().unwrap();
        let config_path = ImageConfig::get_config_file_path(dir.path());
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            r#"
registry = "https://example.com/registry.toml"
local_storage = "/tmp/legacy"
auto_sync = true
auto_sync_threshold = 604800
"#,
        )
        .unwrap();

        let config = ImageConfig::read_config_with_env(dir.path(), |_| None).unwrap();

        assert_eq!(config, ImageConfig::new_default(dir.path()));

        let normalized = fs::read_to_string(config_path).unwrap();
        assert!(!normalized.contains("local_storage"));
        assert!(!normalized.contains("auto_sync"));
        assert_eq!(toml::from_str::<ImageConfig>(&normalized).unwrap(), config);
    }

    #[test]
    fn read_config_removes_unknown_fields() {
        let dir = tempdir().unwrap();
        let config_path = ImageConfig::get_config_file_path(dir.path());
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            r#"
registry = "https://example.com/registry.toml"
download_dir = "/tmp/downloads"
extract_dir = "/tmp/rootfs"
custom_field = "ignored"
"#,
        )
        .unwrap();

        let config = ImageConfig::read_config_with_env(dir.path(), |_| None).unwrap();

        assert_eq!(config.registry, "https://example.com/registry.toml");
        assert_eq!(config.download_dir, PathBuf::from("/tmp/downloads"));
        assert_eq!(config.extract_dir, PathBuf::from("/tmp/rootfs"));
        assert!(
            !fs::read_to_string(config_path)
                .unwrap()
                .contains("custom_field")
        );
    }

    #[test]
    fn read_config_recreates_file_missing_current_fields() {
        let dir = tempdir().unwrap();
        let config_path = ImageConfig::get_config_file_path(dir.path());
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            r#"registry = "https://example.com/registry.toml"
"#,
        )
        .unwrap();

        let config = ImageConfig::read_config_with_env(dir.path(), |_| None).unwrap();

        assert_eq!(config, ImageConfig::new_default(dir.path()));
        assert_eq!(
            toml::from_str::<ImageConfig>(&fs::read_to_string(config_path).unwrap()).unwrap(),
            config
        );
    }

    #[test]
    fn read_config_recreates_file_with_invalid_current_values() {
        let dir = tempdir().unwrap();
        let config_path = ImageConfig::get_config_file_path(dir.path());
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            r#"
registry = 42
download_dir = "/tmp/downloads"
extract_dir = "/tmp/rootfs"
"#,
        )
        .unwrap();

        let config = ImageConfig::read_config_with_env(dir.path(), |_| None).unwrap();

        assert_eq!(config, ImageConfig::new_default(dir.path()));
        assert_eq!(
            toml::from_str::<ImageConfig>(&fs::read_to_string(config_path).unwrap()).unwrap(),
            config
        );
    }
}
