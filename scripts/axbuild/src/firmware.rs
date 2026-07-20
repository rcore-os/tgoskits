//! AIC8800 Wi-Fi firmware provisioning.
//!
//! The AIC8800 firmware blobs are vendor binaries that we do not vendor into
//! the git tree. Instead they are fetched on demand (and integrity-checked
//! against pinned SHA-256 digests) from the upstream LicheeRV Nano firmware
//! package, pinned to a specific commit.
//!
//! This is a workspace-level *cache warmer*: any build/lint/test command that
//! compiles the `aic8800` crate calls [`ensure_aic8800_firmware`] first to
//! populate `components/aic8800/firmware/`. The `aic8800` crate's own
//! `build.rs` then prefers that in-tree dir (so in-repo, incl. offline, builds
//! never hit the network), falling back to its own download only when the dir
//! is absent (e.g. a standalone crates.io build). Keep this manifest in sync
//! with `components/aic8800/build.rs`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::support::download::{download_file_verified_sha256, http_client};

#[path = "../../../components/aic8800/src/firmware_manifest.rs"]
mod firmware_manifest;

use firmware_manifest::{
    COMMIT as FIRMWARE_COMMIT, FILES as FIRMWARE_FILES, FirmwareFile, REPOSITORY as FIRMWARE_REPO,
};

fn firmware_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("components/aic8800/firmware")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Returns `true` if `path` exists and its contents match `expected_sha256`.
fn file_matches(path: &Path, expected_sha256: &str) -> bool {
    match std::fs::read(path) {
        Ok(bytes) => sha256_hex(&bytes) == expected_sha256,
        Err(_) => false,
    }
}

/// Ensures every AIC8800 firmware blob is present under
/// `components/aic8800/firmware/` with the expected contents, downloading any
/// missing or mismatched file from the pinned upstream commit.
///
/// Idempotent and cheap when blobs are already in place (only hashing, no
/// network). Safe to call before any command that compiles `aic8800`.
pub async fn ensure_aic8800_firmware(workspace_root: &Path) -> Result<()> {
    let dir = firmware_dir(workspace_root);

    let missing: Vec<&FirmwareFile> = FIRMWARE_FILES
        .iter()
        .filter(|f| !file_matches(&dir.join(f.name), f.sha256))
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create firmware dir {}", dir.display()))?;

    let client = http_client()?;
    log::info!(
        "fetching {} AIC8800 firmware blob(s) from {}@{}",
        missing.len(),
        FIRMWARE_REPO,
        &FIRMWARE_COMMIT[..12]
    );

    for file in missing {
        let url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            FIRMWARE_REPO, FIRMWARE_COMMIT, file.remote_path
        );
        let dest = dir.join(file.name);
        download_file_verified_sha256(&client, &url, &dest, file.sha256)
            .await
            .with_context(|| format!("failed to fetch firmware {} from {url}", file.name))?;
        log::info!("  fetched {}", file.name);
    }

    Ok(())
}
