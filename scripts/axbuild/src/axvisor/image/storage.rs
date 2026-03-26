use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;

use super::{ImageConfig, registry::ImageRegistry};

pub const REGISTRY_FILENAME: &str = "images.toml";
const LAST_SYNC_FILENAME: &str = ".last_sync";

#[derive(Debug)]
pub struct Storage {
    pub path: PathBuf,
    pub image_registry: ImageRegistry,
}

impl Storage {
    pub fn new(path: PathBuf) -> anyhow::Result<Self> {
        let registry_filepath = registry_filepath(&path);
        let image_registry = ImageRegistry::load_from_file(&registry_filepath)?;
        Ok(Self {
            path,
            image_registry,
        })
    }

    pub async fn new_from_registry(registry: String, path: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&path).map_err(|e| anyhow!("Failed to create directory: {e}"))?;
        let image_registry = ImageRegistry::fetch_with_includes(&registry).await?;
        Self::write_registry_to_path(path, image_registry)
    }

    fn write_registry_to_path(
        path: PathBuf,
        image_registry: ImageRegistry,
    ) -> anyhow::Result<Self> {
        let registry_filepath = registry_filepath(&path);
        let toml_content = toml::to_string_pretty(&image_registry)
            .map_err(|e| anyhow!("Failed to serialize registry: {e}"))?;
        fs::write(&registry_filepath, toml_content)
            .map_err(|e| anyhow!("Failed to write registry file: {e}"))?;
        write_last_sync_time(&path)?;
        Ok(Self {
            path,
            image_registry,
        })
    }

    pub async fn new_with_auto_sync(
        path: PathBuf,
        registry: String,
        auto_sync_threshold: u64,
    ) -> anyhow::Result<Self> {
        let storage = match Self::new(path.clone()) {
            Ok(storage) => storage,
            Err(err) => {
                println!("Error while loading local storage: {err}");
                println!("Auto syncing from registry {registry}...");
                return Self::new_from_registry(registry, path).await;
            }
        };

        if auto_sync_threshold == 0 {
            return Ok(storage);
        }

        let now = current_unix_timestamp()?;
        let last_sync = read_last_sync_time(&storage.path);
        let need_sync = match last_sync {
            None => true,
            Some(ts) => now.saturating_sub(ts) >= auto_sync_threshold,
        };
        if !need_sync {
            return Ok(storage);
        }

        let registry_path = registry_filepath(&storage.path);
        let backup = fs::read_to_string(&registry_path)
            .with_context(|| format!("Failed to read {}", registry_path.display()))?;
        match Self::new_from_registry(registry, path).await {
            Ok(storage) => Ok(storage),
            Err(err) => {
                println!("Auto sync failed: {err}");
                fs::write(&registry_path, backup)
                    .with_context(|| format!("Failed to restore {}", registry_path.display()))?;
                Self::new(storage.path)
            }
        }
    }

    pub async fn new_from_config(config: &ImageConfig) -> anyhow::Result<Self> {
        if config.auto_sync {
            Self::new_with_auto_sync(
                config.local_storage.clone(),
                config.registry.clone(),
                config.auto_sync_threshold,
            )
            .await
        } else {
            Self::new(config.local_storage.clone())
        }
    }

    #[cfg(test)]
    async fn new_with_auto_sync_for_test(
        path: PathBuf,
        auto_sync_threshold: u64,
        image_registry: ImageRegistry,
    ) -> anyhow::Result<Self> {
        let storage = match Self::new(path.clone()) {
            Ok(storage) => storage,
            Err(_) => return Self::write_registry_to_path(path, image_registry),
        };

        if auto_sync_threshold == 0 {
            return Ok(storage);
        }

        let now = current_unix_timestamp()?;
        let last_sync = read_last_sync_time(&storage.path);
        let need_sync = match last_sync {
            None => true,
            Some(ts) => now.saturating_sub(ts) >= auto_sync_threshold,
        };
        if !need_sync {
            return Ok(storage);
        }

        Self::write_registry_to_path(path, image_registry)
    }
}

fn registry_filepath(storage_path: &Path) -> PathBuf {
    storage_path.join(REGISTRY_FILENAME)
}

fn last_sync_filepath(storage_path: &Path) -> PathBuf {
    storage_path.join(LAST_SYNC_FILENAME)
}

fn current_unix_timestamp() -> anyhow::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow!("System time error: {e}"))
        .map(|d| d.as_secs())
}

fn read_last_sync_time(storage_path: &Path) -> Option<u64> {
    let path = last_sync_filepath(storage_path);
    let s = fs::read_to_string(path).ok()?;
    s.trim().parse::<u64>().ok()
}

fn write_last_sync_time(storage_path: &Path) -> anyhow::Result<()> {
    let now = current_unix_timestamp()?;
    fs::write(last_sync_filepath(storage_path), now.to_string())
        .map_err(|e| anyhow!("Failed to write last sync file: {e}"))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn sample_registry() -> &'static str {
        r#"
[[images]]
name = "linux"
version = "0.0.1"
released_at = "2025-01-01T00:00:00Z"
description = "Linux guest"
sha256 = "abc"
arch = "aarch64"
url = "https://example.com/linux-0.0.1.tar.gz"
"#
    }

    #[test]
    fn loads_local_registry() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(dir.path().join(REGISTRY_FILENAME), sample_registry()).unwrap();

        let storage = Storage::new(dir.path().to_path_buf()).unwrap();

        assert_eq!(storage.image_registry.images.len(), 1);
        assert_eq!(storage.image_registry.images[0].name, "linux");
    }

    #[tokio::test]
    async fn auto_sync_fetches_registry_when_missing() {
        let dir = tempdir().unwrap();
        let sample = dir.path().join("sample.toml");
        fs::write(&sample, sample_registry()).unwrap();
        let image_registry = ImageRegistry::load_from_file(&sample).unwrap();

        let storage =
            Storage::new_with_auto_sync_for_test(dir.path().to_path_buf(), 60, image_registry)
                .await
                .unwrap();

        assert_eq!(storage.image_registry.images.len(), 1);
        assert!(dir.path().join(REGISTRY_FILENAME).exists());
    }

    #[test]
    fn config_without_auto_sync_requires_local_registry() {
        let dir = tempdir().unwrap();
        let config = ImageConfig {
            local_storage: dir.path().to_path_buf(),
            registry: "https://example.com/registry.toml".to_string(),
            auto_sync: false,
            auto_sync_threshold: 60,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(Storage::new_from_config(&config)).unwrap_err();

        assert!(err.to_string().contains("Failed to read image registry"));
    }
}
