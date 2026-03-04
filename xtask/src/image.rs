//! xtask/src/image.rs
//! Guest Image management commands for the Axvisor build configuration tool
//! (https://github.com/arceos-hypervisor/xtask).
//!
//! This module provides functionality to list, download, and remove
//! pre-built guest images for various supported boards and architectures. The images
//! are downloaded from a specified URL base and verified using SHA-256 checksums. The downloaded
//! images are automatically extracted to a specified output directory. Images can also be removed
//! from the temporary directory.
//! ! Usage examples:
//!! ```
//! // List available images
//! xtask image ls
//! // Download a specific image and automatically extract it (default behavior)
//! xtask image download evm3588_arceos --output-dir ./images
//! // Download a specific image without extracting
//! xtask image download evm3588_arceos --output-dir ./images --no-extract
//! // Remove a specific image from temp directory
//! xtask image rm evm3588_arceos
//! ```

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::Read;
use std::path::Path;
use tar::Archive;
use tokio::io::{AsyncWriteExt, BufWriter};

/// Base URL for downloading images
const IMAGE_URL_BASE: &str =
    "https://github.com/arceos-hypervisor/axvisor-guest/releases/download/v0.0.22/";

/// Image management command line arguments.
#[derive(Parser)]
pub struct ImageArgs {
    #[command(subcommand)]
    pub command: ImageCommands,
}

/// Image management commands
#[derive(Subcommand)]
pub enum ImageCommands {
    /// List all available images
    Ls,
    /// Download the specified image and automatically extract it
    Download {
        /// Name of the image to download
        image_name: String,
        /// Output directory for the downloaded image
        #[arg(short, long)]
        output_dir: Option<String>,
        /// Do not extract after download
        #[arg(long, help = "Do not extract after download")]
        no_extract: bool,
    },
    /// Remove the specified image from temp directory
    Rm {
        /// Name of the image to remove
        image_name: String,
    },
}

/// Representation of a guest image
#[derive(Debug, Clone, Copy)]
struct Image {
    pub name: &'static str,
    pub description: &'static str,
    pub sha256: &'static str,
    pub arch: &'static str,
}

/// Supported guest images
impl Image {
    pub const EVM3588_ARCEOS: Self = Self {
        name: "evm3588_arceos",
        description: "ArceOS for EVM3588 development board",
        sha256: "b1627aa216a98ffa56ce2295252e4d293125184ca394d5e409a1c711bd9a8eff",
        arch: "aarch64",
    };

    pub const EVM3588_LINUX: Self = Self {
        name: "evm3588_linux",
        description: "Linux for EVM3588 development board",
        sha256: "bce8a1a0ed22215f6d98456574009ad4678c6dfda7c5fe8dfcdf15d59f858ef1",
        arch: "aarch64",
    };

    pub const ORANGEPI_ARCEOS: Self = Self {
        name: "orangepi_arceos",
        description: "ArceOS for Orange Pi development board",
        sha256: "cdef0112156bf1d630f8d46d1fd60f45790f99cb8a656a8b716a9efe7958dbc1",
        arch: "aarch64",
    };

    pub const ORANGEPI_LINUX: Self = Self {
        name: "orangepi_linux",
        description: "Linux for Orange Pi development board",
        sha256: "f50a6eaf730a5e962f0ded5474b32cc0b6905e1ee796ac77a1cf83d0576ffbb9",
        arch: "aarch64",
    };

    pub const PHYTIUMPI_ARCEOS: Self = Self {
        name: "phytiumpi_arceos",
        description: "ArceOS for Phytium Pi development board",
        sha256: "8b7b76d60f2868eeaf6b5d28c067bef6307372f31b56b11bbc7999c635d614e5",
        arch: "aarch64",
    };

    pub const PHYTIUMPI_LINUX: Self = Self {
        name: "phytiumpi_linux",
        description: "Linux for Phytium Pi development board",
        sha256: "792d5a6645f773226f6758133b2be4887eceaf713321dcfec7fdffe4412bc46d",
        arch: "aarch64",
    };

    pub const PHYTIUMPI_RTTHREAD: Self = Self {
        name: "phytiumpi_rtthread",
        description: "RT-Thread for Phytium Pi development board",
        sha256: "96ec4bfce212a183b0ca3c6330a815c9e273b393da753d871498672c38815dd8",
        arch: "aarch64",
    };

    pub const QEMU_AARCH64_ARCEOS: Self = Self {
        name: "qemu_aarch64_arceos",
        description: "ArceOS for QEMU aarch64 virtualization",
        sha256: "f2422b89de216e595256b1d18b9305cc1f6d1f3b4d3e50016c50ac9126bf804b",
        arch: "aarch64",
    };

    pub const QEMU_AARCH64_LINUX: Self = Self {
        name: "qemu_aarch64_linux",
        description: "Linux for QEMU aarch64 virtualization",
        sha256: "9e2a19e00a72962c4aac53ad8d6172ca473a93d321e3bb3f49a979264754e397",
        arch: "aarch64",
    };

    pub const QEMU_AARCH64_NIMBOS: Self = Self {
        name: "qemu_aarch64_nimbos",
        description: "NIMBOS for QEMU aarch64 virtualization",
        sha256: "77d50bc1b5df282e31ed3780dd2d09691026836144c0ce6f46ef99301f5aaea8",
        arch: "aarch64",
    };

    pub const QEMU_RISCV64_ARCEOS: Self = Self {
        name: "qemu_riscv64_arceos",
        description: "ArceOS for QEMU riscv64 virtualization",
        sha256: "1c6a63c1ff1713aa7aaac61a07c2c43e94cace404a8f9e5808275d5335526b0b",
        arch: "riscv64",
    };

    pub const QEMU_RISCV64_LINUX: Self = Self {
        name: "qemu_riscv64_linux",
        description: "Linux for QEMU riscv64 virtualization",
        sha256: "86ec223dcdd0cd39210e961d1d6cac0179f03c9a179b6514cd787410bf766d61",
        arch: "riscv64",
    };

    pub const QEMU_RISCV64_NIMBOS: Self = Self {
        name: "qemu_riscv64_nimbos",
        description: "NIMBOS for QEMU riscv64 virtualization",
        sha256: "ce75be7bb5dc6ca1df3a23a055100adbd309da0cc65e0dc669a35f81e369249f",
        arch: "riscv64",
    };

    pub const QEMU_X86_64_ARCEOS: Self = Self {
        name: "qemu_x86_64_arceos",
        description: "ArceOS for QEMU x86_64 virtualization",
        sha256: "64538c2330e7658a381902af448cda72e14d9b7971995eb6c4ec5cf8bcaa622b",
        arch: "x86_64",
    };

    pub const QEMU_X86_64_LINUX: Self = Self {
        name: "qemu_x86_64_linux",
        description: "Linux for QEMU x86_64 virtualization",
        sha256: "8a04e36174dd0b0ea386c261d1fee59d47cfbd2e5f0f2a48c53f99c4deead0a5",
        arch: "x86_64",
    };

    pub const QEMU_X86_64_NIMBOS: Self = Self {
        name: "qemu_x86_64_nimbos",
        description: "NIMBOS for QEMU x86_64 virtualization",
        sha256: "dddfa6067af453d49f2925fe75514779549041ba78bb390a54aab1c836ed6c63",
        arch: "x86_64",
    };

    pub const ROC_RK3568_PC_ARCEOS: Self = Self {
        name: "roc-rk3568-pc_arceos",
        description: "ArceOS for ROC-RK3568-PC development board",
        sha256: "5ec4721d449d302ce6e00ec6efeb8e5284e1ed2051e4a417229a1464d991469c",
        arch: "aarch64",
    };

    pub const ROC_RK3568_PC_LINUX: Self = Self {
        name: "roc-rk3568-pc_linux",
        description: "Linux for ROC-RK3568-PC development board",
        sha256: "1c47b0fa63f47c6e02450a4c4bac59d74c486a55f72b032c1cf5f4a432e2973e",
        arch: "aarch64",
    };

    pub const TAC_E400_PLC_ARCEOS: Self = Self {
        name: "tac-e400-plc_arceos",
        description: "ArceOS for TAC-E400-PLC industrial control board",
        sha256: "0b249ff8daeb1070b4518f09ef46b05266322594907afd0ed2b21b83810ca6ab",
        arch: "aarch64",
    };

    pub const TAC_E400_PLC_LINUX: Self = Self {
        name: "tac-e400-plc_linux",
        description: "Linux for TAC-E400-PLC industrial control board",
        sha256: "a639c5bdb2d1ed07907085e2d181be2d5f2f5433f7fe64bcae8d8d86ddf657f7",
        arch: "aarch64",
    };

    /// Get all supported images
    pub fn all() -> &'static [Image] {
        &[
            Self::EVM3588_ARCEOS,
            Self::EVM3588_LINUX,
            Self::ORANGEPI_ARCEOS,
            Self::ORANGEPI_LINUX,
            Self::PHYTIUMPI_ARCEOS,
            Self::PHYTIUMPI_LINUX,
            Self::PHYTIUMPI_RTTHREAD,
            Self::QEMU_AARCH64_ARCEOS,
            Self::QEMU_RISCV64_ARCEOS,
            Self::QEMU_X86_64_ARCEOS,
            Self::QEMU_AARCH64_LINUX,
            Self::QEMU_RISCV64_LINUX,
            Self::QEMU_X86_64_LINUX,
            Self::QEMU_AARCH64_NIMBOS,
            Self::QEMU_RISCV64_NIMBOS,
            Self::QEMU_X86_64_NIMBOS,
            Self::ROC_RK3568_PC_ARCEOS,
            Self::ROC_RK3568_PC_LINUX,
            Self::TAC_E400_PLC_ARCEOS,
            Self::TAC_E400_PLC_LINUX,
        ]
    }

    /// Find image by name
    pub fn find_by_name(name: &str) -> Option<&'static Image> {
        Self::all().iter().find(|image| image.name == name)
    }
}

/// Verify the SHA256 checksum of a file
/// # Arguments
/// * `file_path` - The path to the file to verify
/// * `expected_sha256` - The expected SHA256 checksum as a hex string
/// # Returns
/// * `Result<bool>` - Result indicating whether the checksum matches
/// # Errors
/// * `anyhow::Error` - If any error occurs during the verification process
fn image_verify_sha256(file_path: &Path, expected_sha256: &str) -> Result<bool> {
    let mut file = fs::File::open(file_path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let result = hasher.finalize();
    let actual_sha256 = format!("{result:x}");

    Ok(actual_sha256 == expected_sha256)
}

/// List all available images
/// # Returns
/// * `Result<()>` - Result indicating success or failure
/// # Errors
/// * `anyhow::Error` - If any error occurs during the listing process
/// # Examples
/// ```
/// // List all available images
/// xtask image ls
/// ```
fn image_list() -> Result<()> {
    // Retrieve all images from the database or storage
    let images = Image::all();

    // Print table headers with specific column widths
    println!(
        "{:<25} {:<30} {:<50}",
        "Name", "Architecture", "Description"
    );
    // Print a separator line for better readability
    println!("{}", "-".repeat(90));

    // Iterate through each image and print its details
    for image in images {
        // Print image information formatted to match column widths
        println!(
            "{:<25} {:<15} {:<50}",
            // Image name
            image.name,
            // Architecture type
            image.arch,
            image.description
        );
    }

    Ok(())
}

/// Download the specified image and optionally extract it
/// # Arguments
/// * `image_name` - The name of the image to download
/// * `output_dir` - Optional output directory to save the downloaded image
/// * `extract` - Whether to automatically extract the image after download (default: true)
/// # Returns
/// * `Result<()>` - Result indicating success or failure
/// # Errors
/// * `anyhow::Error` - If any error occurs during the download or extraction process
/// # Examples
/// ```
/// // Download the evm3588_arceos image to the ./images directory and automatically extract it
/// xtask image download evm3588_arceos --output-dir ./images
/// ```
async fn image_download(image_name: &str, output_dir: Option<String>, extract: bool) -> Result<()> {
    let image = Image::find_by_name(image_name).ok_or_else(|| {
        anyhow!("Image not found: {image_name}. Use 'xtask image ls' to view available images")
    })?;

    let output_path = match output_dir {
        Some(dir) => {
            // Check if it's an absolute path
            let path = Path::new(&dir);
            if path.is_absolute() {
                // If it's an absolute path, use it directly
                path.join(format!("{image_name}.tar.gz"))
            } else {
                // If it's a relative path, base on current working directory
                let current_dir = std::env::current_dir()?;
                current_dir.join(path).join(format!("{image_name}.tar.gz"))
            }
        }
        None => {
            // If not specified, use system temporary directory
            let temp_dir = env::temp_dir();
            temp_dir
                .join("axvisor")
                .join(format!("{image_name}.tar.gz"))
        }
    };

    // Check if file exists, if so verify SHA256
    if output_path.exists() {
        match image_verify_sha256(&output_path, image.sha256) {
            Ok(true) => {
                println!("Image already exists and verified");
                return Ok(());
            }
            Ok(false) => {
                println!("Existing image verification failed, re-downloading");
                // Remove the invalid file before downloading
                let _ = fs::remove_file(&output_path);
            }
            Err(_) => {
                println!("Error verifying existing image, re-downloading");
                // Remove the potentially corrupted file before downloading
                let _ = fs::remove_file(&output_path);
            }
        }
    }

    // Ensure target directory exists
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Build download URL
    let download_url = format!("{}{}.tar.gz", IMAGE_URL_BASE, image.name);
    println!("Downloading: {download_url}");

    // Use reqwest to download the file
    let mut response = reqwest::get(&download_url).await?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to download file: HTTP {}",
            response.status()
        ));
    }

    // Create file with buffered writer for efficient streaming
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&output_path)
        .await?;
    let mut writer = BufWriter::new(file);

    // Get content length for progress reporting (if available)
    let content_length = response.content_length();
    let mut downloaded = 0u64;

    // Stream the response body to file using chunks
    while let Some(chunk) = response.chunk().await? {
        // Write chunk to file
        writer
            .write_all(&chunk)
            .await
            .map_err(|e| anyhow!("Error writing to file: {e}"))?;

        // Update progress
        downloaded += chunk.len() as u64;
        if let Some(total) = content_length {
            let percent = (downloaded * 100) / total;
            print!("\rDownloading: {percent}% ({downloaded}/{total} bytes)");
        } else {
            print!("\rDownloaded: {downloaded} bytes");
        }
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
    }

    // Flush the writer to ensure all data is written to disk
    writer
        .flush()
        .await
        .map_err(|e| anyhow!("Error flushing file: {e}"))?;

    // Verify downloaded file
    match image_verify_sha256(&output_path, image.sha256) {
        Ok(true) => {
            println!("Download completed and verified successfully");
        }
        Ok(false) => {
            // Remove the invalid downloaded file
            let _ = fs::remove_file(&output_path);
            return Err(anyhow!(
                "Download completed but file SHA256 verification failed"
            ));
        }
        Err(e) => {
            // Remove the potentially corrupted downloaded file
            let _ = fs::remove_file(&output_path);
            return Err(anyhow!(
                "Download completed but error verifying downloaded file: {e}"
            ));
        }
    }

    // If extract flag is true, extract the downloaded file
    if extract {
        println!("Extracting image...");

        // Determine extraction output directory
        let extract_dir = output_path
            .parent()
            .ok_or_else(|| anyhow!("Unable to determine parent directory of downloaded file"))?
            .join(image_name);

        // Ensure extraction directory exists
        fs::create_dir_all(&extract_dir)?;

        // Open the compressed tar file
        let tar_gz = fs::File::open(&output_path)?;
        let decoder = GzDecoder::new(tar_gz);
        let mut archive = Archive::new(decoder);

        // Extract the archive
        archive.unpack(&extract_dir)?;

        println!("Image extracted to: {}", extract_dir.display());
    }

    Ok(())
}

/// Remove the specified image from temp directory
/// # Arguments
/// * `image_name` - The name of the image to remove
/// # Returns
/// * `Result<()>` - Result indicating success or failure
/// # Errors
/// * `anyhow::Error` - If any error occurs during the removal process
/// # Examples
/// ```
/// // Remove the evm3588_arceos image from temp directory
/// xtask image rm evm3588_arceos
/// ```
fn image_remove(image_name: &str) -> Result<()> {
    // Check if the image name is valid by looking it up
    let _image = Image::find_by_name(image_name).ok_or_else(|| {
        anyhow!("Image not found: {image_name}. Use 'xtask image ls' to view available images")
    })?;

    let temp_dir = env::temp_dir().join("axvisor");
    let tar_file = temp_dir.join(format!("{image_name}.tar.gz"));
    let extract_dir = temp_dir.join(image_name);

    let mut removed = false;

    // Remove the tar file if it exists
    if tar_file.exists() {
        fs::remove_file(&tar_file)?;
        removed = true;
    }

    // Remove the extracted directory if it exists
    if extract_dir.exists() {
        fs::remove_dir_all(&extract_dir)?;
        removed = true;
    }

    if !removed {
        println!("No files found for image: {image_name}");
    } else {
        println!("Image removed successfully");
    }

    Ok(())
}

/// Main function to run image management commands
/// # Arguments
/// * `args` - The image command line arguments
/// # Returns
/// * `Result<()>` - Result indicating success or failure
/// # Errors
/// * `anyhow::Error` - If any error occurs during command execution
/// # Examples
/// ```
/// // Run image management commands
/// xtask image ls
/// xtask image download evm3588_arceos --output-dir ./images
/// xtask image rm evm3588_arceos
/// ```
pub async fn run_image(args: ImageArgs) -> Result<()> {
    match args.command {
        ImageCommands::Ls => {
            image_list()?;
        }
        ImageCommands::Download {
            image_name,
            output_dir,
            no_extract,
        } => {
            // Determine if extraction should be performed
            let should_extract = !no_extract;
            image_download(&image_name, output_dir, should_extract).await?;
        }
        ImageCommands::Rm { image_name } => {
            image_remove(&image_name)?;
        }
    }

    Ok(())
}
