use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const DEFAULT_REGISTRY_URL: &str = "https://raw.githubusercontent.com/arceos-hypervisor/axvisor-guest/refs/heads/main/registry/default.toml";
pub const DEFAULT_FALLBACK_REGISTRY_URL: &str = "https://raw.githubusercontent.com/arceos-hypervisor/axvisor-guest/refs/heads/main/registry/v0.0.25.toml";
pub const IMAGE_CONFIG_FILENAME: &str = ".image.toml";
const DEFAULT_AUTO_SYNC_THRESHOLD: u64 = 60 * 60 * 24 * 7;
const LOCAL_STORAGE_ENV: &str = "AXVISOR_IMAGE_LOCAL_STORAGE";

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

        let mut config = if !path.exists() {
            let config = Self::new_default();
            Self::write_config(base_dir, &config)?;
            config
        } else {
            let s = fs::read_to_string(&path)?;
            toml::from_str(&s).map_err(|e| anyhow!("Invalid image config file: {e}"))?
        };

        if let Ok(local_storage) = std::env::var(LOCAL_STORAGE_ENV)
            && !local_storage.trim().is_empty()
        {
            config.local_storage = PathBuf::from(local_storage);
        }

        Ok(config)
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
    use std::{
        ffi::{OsStr, OsString},
        sync::{LazyLock, Mutex},
    };

    use tempfile::tempdir;

    use super::*;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    struct TempEnvVar {
        key: &'static str,
        original: Option<OsString>,
    }

    impl TempEnvVar {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }

        fn unset(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, original }
        }
    }

    impl Drop for TempEnvVar {
        fn drop(&mut self) {
            match self.original.as_ref() {
                Some(value) => unsafe {
                    std::env::set_var(self.key, value);
                },
                None => unsafe {
                    std::env::remove_var(self.key);
                },
            }
        }
    }

    #[test]
    fn read_config_creates_default_when_missing() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = TempEnvVar::unset(LOCAL_STORAGE_ENV);
        let dir = tempdir().unwrap();

        let config = ImageConfig::read_config(dir.path()).unwrap();

        assert_eq!(config, ImageConfig::new_default());
        assert!(ImageConfig::get_config_file_path(dir.path()).exists());
    }

    #[test]
    fn read_config_prefers_local_storage_env_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let override_path = dir.path().join("persistent-cache");
        let _env = TempEnvVar::set(LOCAL_STORAGE_ENV, override_path.as_os_str());

        let config = ImageConfig::read_config(dir.path()).unwrap();

        assert_eq!(config.local_storage, override_path);
    }
}
