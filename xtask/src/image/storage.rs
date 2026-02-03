//! Local image storage management.
//!
//! Provides `Storage` for managing a local image directory and its registry index.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, path::PathBuf};

use anyhow::{Result, anyhow};

use super::config::ImageConfig;
use super::download::{download_to_path, image_verify_sha256};
use super::registry::{ImageEntry, ImageRegistry};
use super::spec::ImageSpecRef;

/// Filename of the image registry index inside the local storage directory.
pub const REGISTRY_FILENAME: &str = "images.toml";

/// Filename storing the last sync timestamp (Unix seconds) inside the local storage directory.
const LAST_SYNC_FILENAME: &str = ".last_sync";

// -----------------------------------------------------------------------------
// Path and naming helpers (free functions, no Storage instance needed)
// -----------------------------------------------------------------------------

/// Returns the path to the registry index file within a storage directory.
///
/// # Arguments
///
/// * `storage_path` - Root path of the local image storage
pub fn registry_filepath(storage_path: &Path) -> PathBuf {
    storage_path.join(REGISTRY_FILENAME)
}

/// Path to `.last_sync` file in the storage directory.
fn last_sync_filepath(storage_path: &Path) -> PathBuf {
    storage_path.join(LAST_SYNC_FILENAME)
}

/// Current time as Unix timestamp (seconds).
fn current_unix_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow!("System time error: {e}"))
        .map(|d| d.as_secs())
}

/// Reads the last sync timestamp from `.last_sync`; returns `None` if missing or invalid.
fn read_last_sync_time(storage_path: &Path) -> Option<u64> {
    let path = last_sync_filepath(storage_path);
    if !path.exists() {
        return None;
    }
    let s = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            println!(
                "Note: could not read last sync file {}: {e}; treating as no previous sync.",
                path.display()
            );
            return None;
        }
    };
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    match s.parse::<u64>() {
        Ok(ts) => Some(ts),
        Err(_) => {
            println!(
                "Note: last sync file {} has invalid content; treating as no previous sync.",
                path.display()
            );
            None
        }
    }
}

/// Writes the current timestamp to `.last_sync`.
fn write_last_sync_time(storage_path: &Path) -> Result<()> {
    let now = current_unix_timestamp()?;
    let path = last_sync_filepath(storage_path);
    fs::write(&path, now.to_string()).map_err(|e| anyhow!("Failed to write last sync file: {e}"))
}

/// Canonical archive filename for an image: `{name}.tar.gz` or `{name}-{version}.tar.gz`.
///
/// # Arguments
///
/// * `spec` - Image spec (name and optional version)
pub fn image_archive_filename(spec: ImageSpecRef<'_>) -> String {
    match spec.version {
        Some(v) => format!("{}-{}.tar.gz", spec.name, v),
        None => format!("{}.tar.gz", spec.name),
    }
}

/// Canonical extract directory name for an image: `{name}` or `{name}-{version}`.
///
/// # Arguments
///
/// * `spec` - Image spec (name and optional version)
pub fn image_extract_dir_name(spec: ImageSpecRef<'_>) -> String {
    match spec.version {
        Some(v) => format!("{}-{}", spec.name, v),
        None => spec.name.to_string(),
    }
}

/// Returns the path where an image archive (`.tar.gz`) would be stored.
pub fn image_path(storage_path: &Path, spec: ImageSpecRef<'_>) -> PathBuf {
    storage_path.join(image_archive_filename(spec))
}

/// Local image storage backed by a directory and an image registry index.
pub struct Storage {
    /// Root path of the local image storage directory.
    pub path: PathBuf,
    /// Parsed image registry (list of available images).
    pub image_registry: ImageRegistry,
}

// -----------------------------------------------------------------------------
// Construction
// -----------------------------------------------------------------------------

impl Storage {
    /// Creates a storage instance from an existing local directory.
    ///
    /// Loads the image registry from `images.toml` in the storage path.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the local storage directory (must contain `images.toml`)
    ///
    /// # Returns
    ///
    /// * `Ok(Self)` - Storage loaded successfully
    /// * `Err` - Directory or registry file read/parse error
    pub fn new(path: PathBuf) -> Result<Self> {
        let registry_filepath = registry_filepath(&path);
        let image_registry = ImageRegistry::load_from_file(&registry_filepath)?;
        Ok(Self {
            path,
            image_registry,
        })
    }

    /// Creates storage by downloading the registry index from the remote URL. This method does not
    /// affect existing images in the local storage.
    ///
    /// If the registry TOML contains `[[includes]]`, those URLs are fetched recursively
    /// and merged (deduplicated by name+version). The local saved registry has no `includes`.
    ///
    /// # Arguments
    ///
    /// * `registry` - URL of the registry TOML file to download
    /// * `path` - Path to the local storage directory
    ///
    /// # Returns
    ///
    /// * `Ok(Self)` - Registry downloaded and storage created
    /// * `Err` - Download, directory creation, or parse error
    pub async fn new_from_registry(registry: String, path: PathBuf) -> Result<Self> {
        fs::create_dir_all(&path).map_err(|e| anyhow!("Failed to create directory: {e}"))?;

        let registry_filepath = registry_filepath(&path);

        let image_registry = ImageRegistry::fetch_with_includes(&registry).await?;
        let toml_content = toml::to_string_pretty(&image_registry)
            .map_err(|e| anyhow!("Failed to serialize registry: {e}"))?;
        fs::write(&registry_filepath, toml_content)
            .map_err(|e| anyhow!("Failed to write registry file: {e}"))?;
        write_last_sync_time(&path)?;

        println!("Image list saved to {}", registry_filepath.display());

        Ok(Self {
            path,
            image_registry,
        })
    }

    /// Creates storage, falling back to syncing from the remote registry if local load fails.
    /// When local load succeeds and `auto_sync_threshold` is non-zero, checks the last sync
    /// time (stored in `.last_sync` under the storage path) and syncs from the remote registry
    /// if the threshold in seconds has been exceeded (or no last sync time exists).
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the local storage directory
    /// * `registry` - URL of the remote registry to sync from when local storage is invalid or stale
    /// * `auto_sync_threshold` - Seconds since last sync before auto-updating; 0 means never update when load succeeds
    ///
    /// # Returns
    ///
    /// * `Ok(Self)` - Storage from local dir or from synced registry
    /// * `Err` - Both local load and sync failed
    pub async fn new_with_auto_sync(
        path: PathBuf,
        registry: String,
        auto_sync_threshold: u64,
    ) -> Result<Self> {
        let storage = match Self::new(path.clone()) {
            Ok(storage) => storage,
            Err(e) => {
                println!("Error while loading local storage: {e}");
                println!("Auto syncing from registry {registry}...");
                let storage = Self::new_from_registry(registry, path).await?;
                return Ok(storage);
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

        println!(
            "Last sync was {} (threshold: {}s). Auto syncing from registry {registry}...",
            last_sync
                .map(|ts| format!("{}s ago", now - ts))
                .unwrap_or_else(|| "never".to_string()),
            auto_sync_threshold
        );

        // backup registry file so we can restore on sync failure.
        let registry_path = registry_filepath(&storage.path);
        let registry_backup = fs::read_to_string(&registry_path)
            .map_err(|e| anyhow!("Failed to read registry file: {e}"))?;

        match Self::new_from_registry(registry, path).await {
            Ok(new_storage) => Ok(new_storage),
            Err(e) => {
                println!("Auto sync failed: {e}");
                println!("Restoring previous registry and using existing storage.");

                fs::write(&registry_path, registry_backup)
                    .map_err(|e| anyhow!("Failed to write registry file: {e}"))?;

                Ok(storage)
            }
        }
    }

    /// Creates storage from config, optionally auto-syncing when local storage is invalid.
    ///
    /// # Arguments
    ///
    /// * `config` - Image config (storage path, registry URL, auto-sync settings)
    ///
    /// # Returns
    ///
    /// * `Ok(Self)` - Storage loaded or synced according to config
    /// * `Err` - Load or sync failed
    pub async fn new_from_config(config: &ImageConfig) -> Result<Self> {
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
}

// -----------------------------------------------------------------------------
// Download and remove
// -----------------------------------------------------------------------------

impl Storage {
    /// Resolves an image by name and optional version (latest by `released_at` when version is `None`).
    fn resolve_image(&self, spec: ImageSpecRef<'_>) -> Option<&ImageEntry> {
        self.image_registry.find(spec)
    }

    /// Downloads an image into the given directory and verifies its SHA256 checksum.
    /// The output filename is derived from the image spec (see [`image_archive_filename`]).
    ///
    /// Skips download if the file already exists and matches the expected checksum.
    /// Re-downloads on checksum mismatch.
    ///
    /// # Arguments
    ///
    /// * `spec` - Image spec (name and optional version)
    /// * `output_dir` - Directory to write the `.tar.gz` file into (created if missing)
    ///
    /// # Returns
    ///
    /// * `Ok(PathBuf)` - Full path to the downloaded (or existing) image file
    /// * `Err` - Image not found, download failed, or checksum verification failed
    pub async fn download_image_to(
        &self,
        spec: ImageSpecRef<'_>,
        output_dir: &Path,
    ) -> Result<PathBuf> {
        let image = self.resolve_image(spec).ok_or_else(|| {
            anyhow!(
                "Image not found: {}{}. Use 'xtask image ls' to view available images",
                spec.name,
                spec.version
                    .map(|v| format!(" version {}", v))
                    .unwrap_or_default()
            )
        })?;

        fs::create_dir_all(output_dir)
            .map_err(|e| anyhow!("Failed to create output directory: {e}"))?;

        let output_path = output_dir.join(image_archive_filename(spec));

        if output_path.is_dir() {
            return Err(anyhow!(
                "Output path is a directory: {}",
                output_path.display()
            ));
        }

        if output_path.exists() {
            match image_verify_sha256(&output_path, &image.sha256) {
                Ok(true) => {
                    println!("Image already exists and verified");
                    return Ok(output_path);
                }
                Ok(false) => {
                    println!("Existing image verification failed");
                }
                Err(e) => {
                    println!("Error verifying existing image: {e}");
                }
            }

            println!("Removing existing image for re-downloading...");
            let _ = fs::remove_file(&output_path);
        }

        println!("Downloading: {}", image.url);

        download_to_path(&image.url, &output_path, Some("Downloading")).await?;

        match image_verify_sha256(&output_path, &image.sha256) {
            Ok(true) => {
                println!("Download completed and verified successfully");
                Ok(output_path)
            }
            Ok(false) => {
                let err =
                    anyhow!("Image downloaded but verification failed: SHA256 verification failed");
                println!("{err}");
                let _ = fs::remove_file(&output_path);
                Err(err)
            }
            Err(e) => {
                let err =
                    anyhow!("Image downloaded but verification failed: Error verifying image: {e}");
                println!("{err}");
                let _ = fs::remove_file(&output_path);
                Err(err)
            }
        }
    }

    /// Downloads an image to the default location in local storage.
    ///
    /// Equivalent to `download_image_to(spec, &self.path)`.
    ///
    /// # Returns
    ///
    /// * `Ok(PathBuf)` - Full path to the downloaded (or existing) image file
    /// * `Err` - Same as [`download_image_to`](Self::download_image_to)
    pub async fn download_image(&self, spec: ImageSpecRef<'_>) -> Result<PathBuf> {
        self.download_image_to(spec, &self.path).await
    }

    /// Removes an image from local storage (archive and extracted directory).
    ///
    /// # Arguments
    ///
    /// * `spec` - Image spec (name and optional version)
    ///
    /// # Returns
    ///
    /// `true` if at least one file or directory was removed, `false` if none found
    pub async fn remove_image(&self, spec: ImageSpecRef<'_>) -> Result<bool> {
        let mut anything_removed = false;
        let output_path = image_path(&self.path, spec);
        if output_path.exists() {
            fs::remove_file(&output_path)?;
            anything_removed = true;
        }
        let extract_dir = self.path.join(image_extract_dir_name(spec));
        if extract_dir.exists() {
            fs::remove_dir_all(&extract_dir)?;
            anything_removed = true;
        }
        Ok(anything_removed)
    }
}
