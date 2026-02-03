//! Download utilities and checksum verification.
//!
//! Provides async HTTP download with optional progress reporting and SHA256 verification.

use std::{fs, io::Read, path::Path, time::Duration};

use anyhow::{Result, anyhow};
use reqwest::Client;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncWriteExt, BufWriter};

/// HTTP client with 30s connect timeout and 30min total timeout.
fn http_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(1800))
        .build()
        .map_err(|e| anyhow!("Failed to create HTTP client: {e}"))
}

/// Downloads a URL and returns its body as a string.
///
/// # Arguments
///
/// * `url` - URL to download
pub async fn download_to_string(url: &str) -> Result<String> {
    let client = http_client()?;
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(anyhow!("failed to download: HTTP {}", response.status()));
    }
    let body = response.text().await?;
    Ok(body)
}

/// Downloads a URL to a local file, creating parent directories as needed.
///
/// # Arguments
///
/// * `url` - URL to download
/// * `path` - Local path to write the file
/// * `progress_label` - If `Some`, prints download progress (percent/bytes) with this label
pub async fn download_to_path(url: &str, path: &Path, progress_label: Option<&str>) -> Result<()> {
    let client = http_client()?;
    let mut response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(anyhow!("failed to download: HTTP {}", response.status()));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .await?;
    let mut writer = BufWriter::new(file);

    let content_length = response.content_length();
    let mut downloaded = 0u64;

    while let Some(chunk) = response.chunk().await? {
        writer
            .write_all(&chunk)
            .await
            .map_err(|e| anyhow!("Error writing to file: {e}"))?;
        downloaded += chunk.len() as u64;
        if let Some(label) = progress_label {
            if let Some(total) = content_length {
                let percent = (downloaded * 100) / total;
                print!("\r{label}: {percent}% ({downloaded}/{total} bytes)");
            } else {
                print!("\r{label}: {downloaded} bytes");
            }
            std::io::Write::flush(&mut std::io::stdout()).unwrap();
        }
    }

    println!();

    writer
        .flush()
        .await
        .map_err(|e| anyhow!("Error flushing file: {e}"))?;
    Ok(())
}

/// Verifies the SHA256 checksum of a file.
///
/// # Arguments
///
/// * `file_path` - Path to the file to verify
/// * `expected_sha256` - Expected SHA256 checksum as lowercase hex
///
/// # Returns
///
/// * `Ok(true)` - Checksum matches
/// * `Ok(false)` - Checksum does not match
/// * `Err` - I/O or read error during verification
pub fn image_verify_sha256(file_path: &Path, expected_sha256: &str) -> Result<bool> {
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
