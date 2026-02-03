//! Guest Image management commands for the Axvisor build configuration tool
//!
//! This module provides functionality to list, download, and remove
//! pre-built guest images for various supported boards and architectures. The images
//! are downloaded from a specified URL base and verified using SHA-256 checksums. The downloaded
//! images are automatically extracted to a specified output directory. Images can also be removed
//! from the temporary directory.
//!
//! # Usage examples
//!
//! ```
//! // List available images
//! xtask image ls
//! // Download a specific image and automatically extract it (default behavior)
//! xtask image download evm3588_arceos --output-dir ./images
//! // Download a specific image without extracting
//! xtask image download evm3588_arceos --output-dir ./images --no-extract
//! // Remove a specific image from temp directory
//! xtask image rm evm3588_arceos
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use flate2::read::GzDecoder;
use tar::Archive;

mod config;
mod download;
mod registry;
mod spec;
mod storage;

use config::ImageConfig;
use spec::ImageSpecRef;
use storage::Storage;

/// Image management command line arguments.
#[derive(Parser)]
pub struct ImageArgs {
    #[command(flatten)]
    pub overrides: ImageConfigOverrides,

    /// Image subcommand to run: `ls`, `download`, `rm`, or `sync`.
    #[command(subcommand)]
    pub command: ImageCommands,
}

#[derive(Parser)]
pub struct ImageConfigOverrides {
    /// The path to the local storage of images. Override the config file.
    #[arg(short('S'), long, global = true)]
    pub local_storage: Option<PathBuf>,

    /// The URL of the remote registry of images. Override the config file.
    #[arg(short('R'), long, global = true)]
    pub registry: Option<String>,

    /// Do not sync from remote registry even if the local image storage is
    /// broken, missing, or out of date. Override the config file.
    #[arg(short('N'), long, global = true)]
    pub no_auto_sync: bool,

    /// The threshold in seconds to automatically synchronize image list from
    /// remote registry. 0 means never. Override the config file.
    #[arg(long, global = true)]
    pub auto_sync_threshold: Option<u64>,
}

impl ImageConfigOverrides {
    /// Applies CLI overrides onto the given config (in-place).
    ///
    /// # Arguments
    ///
    /// * `config` - Config to mutate; non-`None` override fields overwrite corresponding values
    pub fn apply_on(&self, config: &mut ImageConfig) {
        if let Some(local_storage) = self.local_storage.as_ref() {
            config.local_storage = local_storage.clone();
        }
        if let Some(registry) = self.registry.as_ref() {
            config.registry = registry.clone();
        }
        if self.no_auto_sync {
            config.auto_sync = false;
        }
        if let Some(auto_sync_threshold) = self.auto_sync_threshold {
            config.auto_sync_threshold = auto_sync_threshold;
        }
    }
}

/// Image management commands
#[derive(Subcommand)]
pub enum ImageCommands {
    /// List all available images.
    Ls {
        /// Show different versions of the same image in separate lines.
        #[arg(short, long)]
        verbose: bool,

        /// Filter images by name pattern.
        pattern: Option<String>,
    },

    /// Download the specified image and automatically extract it. Use ASCII
    /// colon to specify version, e.g. `evm3588_arceos:0.0.22`; omit for latest.
    Download {
        /// Image to download: `name` or `name:version`.
        image_name: String,

        /// Output directory for the downloaded image, defaults to
        /// "/tmp/.axvisor-images/".
        #[arg(short, long)]
        output_dir: Option<String>,

        /// Do not extract after download.
        #[arg(long)]
        no_extract: bool,
    },

    /// Remove the specified image from temp directory. Use ASCII colon to
    /// specify version, e.g. `evm3588_arceos:0.0.22`; omit for default path.
    Rm {
        /// Image to remove: `name` or `name:version`.
        image_name: String,
    },

    /// Synchronize image list from a remote registry.
    Sync,

    /// Reset the image config file to default.
    Defconfig,
}

/// Converts a path to absolute; joins with current dir if relative.
fn to_absolute_path(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

/// Returns the path to the AxVisor repository root (parent of the xtask crate).
fn get_axvisor_repo_dir() -> Result<PathBuf> {
    // CARGO_MANIFEST_DIR contains the path of the xtask crate, and we need to
    // get the parent directory to get the AxVisor repository directory.
    Ok(Path::new(&std::env::var("CARGO_MANIFEST_DIR")?)
        .parent()
        .ok_or_else(|| anyhow!("Failed to determine AxVisor repository directory"))?
        .to_path_buf())
}

impl ImageArgs {
    /// Loads image configuration, merging CLI overrides with values from the config file.
    pub async fn get_config(&self) -> Result<ImageConfig> {
        let mut config = ImageConfig::read_config(&get_axvisor_repo_dir()?)?;
        self.overrides.apply_on(&mut config);
        Ok(config)
    }

    /// Executes the selected image subcommand (`ls`, `download`, `rm`, `sync`, or `defconfig`).
    pub async fn execute(&self) -> Result<()> {
        match &self.command {
            ImageCommands::Ls { verbose, pattern } => {
                self.list_images(*verbose, pattern.as_deref()).await?;
            }
            ImageCommands::Download {
                image_name,
                output_dir,
                no_extract,
            } => {
                self.download_image(image_name, output_dir.as_deref(), !no_extract)
                    .await?;
            }
            ImageCommands::Rm { image_name } => {
                self.remove_image(image_name).await?;
            }
            ImageCommands::Sync => {
                self.sync_registry().await?;
            }
            ImageCommands::Defconfig => {
                ImageConfig::reset_config(&get_axvisor_repo_dir()?)?;
            }
        }

        Ok(())
    }

    /// Lists all available images from the local registry to stdout.
    ///
    /// # Arguments
    ///
    /// * `verbose` - If `true`, show each version separately; if `false`, merge same-name images and show version count
    /// * `pattern` - If `Some`, filter by name: try regex match first, fallback to substring
    pub async fn list_images(&self, verbose: bool, pattern: Option<&str>) -> Result<()> {
        let config = self.get_config().await?;
        let storage = Storage::new_from_config(&config).await?;

        storage.image_registry.print(verbose, pattern);

        Ok(())
    }

    /// Downloads the specified image and optionally extracts it.
    ///
    /// # Arguments
    ///
    /// * `spec` - Image spec (name and optional version)
    /// * `output_dir` - If `Some`, write the `.tar.gz` to this directory; if `None`, use config's local storage path
    /// * `extract` - If `true`, extract the archive after download
    pub async fn download_image(
        &self,
        spec: impl Into<ImageSpecRef<'_>>,
        output_dir: Option<&str>,
        extract: bool,
    ) -> Result<()> {
        let spec = spec.into();
        let config = self.get_config().await?;
        let storage = Storage::new_from_config(&config).await?;

        let output_path = match output_dir {
            Some(dir) => {
                storage
                    .download_image_to(spec, &to_absolute_path(Path::new(dir))?)
                    .await?
            }
            None => storage.download_image(spec).await?,
        };

        if extract {
            println!("Extracting image...");

            let extract_dir = output_path
                .parent()
                .ok_or_else(|| anyhow!("Unable to determine parent directory of downloaded file"))?
                .join(storage::image_extract_dir_name(spec));

            fs::create_dir_all(&extract_dir)?;

            let tar_gz = fs::File::open(&output_path)?;
            let decoder = GzDecoder::new(tar_gz);
            let mut archive = Archive::new(decoder);

            archive.unpack(&extract_dir)?;

            println!("Image extracted to: {}", extract_dir.display());
        }
        Ok(())
    }

    /// Removes the specified image from local storage (both `.tar.gz` and extracted directory).
    ///
    /// # Arguments
    ///
    /// * `spec` - Image spec (name and optional version)
    pub async fn remove_image(&self, spec: impl Into<ImageSpecRef<'_>>) -> Result<()> {
        let spec = spec.into();
        let config = self.get_config().await?;
        let storage = Storage::new_from_config(&config).await?;

        if storage.remove_image(spec).await? {
            println!("Image removed successfully");
        } else {
            println!("No files found for image: {}", spec.name);
        }
        Ok(())
    }

    /// Synchronizes the image list from the remote registry to local storage.
    ///
    /// Overwrites the local `images.toml` with the registry contents.
    pub async fn sync_registry(&self) -> Result<()> {
        let config: ImageConfig = self.get_config().await?;
        let _ = Storage::new_from_registry(config.registry, config.local_storage).await?;
        Ok(())
    }
}

/// Dispatches and runs the image subcommand (ls, download, rm, sync) from parsed CLI arguments.
///
/// # Arguments
///
/// * `args` - Parsed image CLI arguments (subcommand and its options)
///
/// # Returns
///
/// * `Ok(())` - Subcommand completed successfully
/// * `Err` - Subcommand failed (e.g. list load, download, checksum, sync, or remove error)
///
/// # Examples
///
/// ```ignore
/// xtask image ls
/// xtask image download evm3588_arceos --output-dir ./images
/// xtask image rm evm3588_arceos
/// ```
pub async fn run_image(args: ImageArgs) -> Result<()> {
    args.execute().await
}
