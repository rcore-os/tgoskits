//! AIC8800 Wi-Fi firmware provisioning.
//!
//! The AIC8800 firmware blobs are vendor binaries that we do not vendor into
//! the git tree. Instead they are fetched on demand (and integrity-checked
//! against pinned SHA-256 digests) from the upstream LicheeRV Nano firmware
//! package, pinned to a specific commit. Any build/lint/test command that
//! compiles the `aic8800` crate calls [`ensure_aic8800_firmware`] first so the
//! `include_bytes!` paths in `components/aic8800/src/fw/firmware/data.rs`
//! resolve without the blobs ever living in version control.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Upstream firmware source: the repo referenced by the LicheeRV Nano
/// buildroot package `aic8800-sdio-firmware`, pinned to a fixed commit.
const FIRMWARE_REPO: &str = "lxowalle/aic8800-sdio-firmware";
const FIRMWARE_COMMIT: &str = "c56f910044cc854d6c553bcb9a644f3bca5a4c38";

/// One firmware blob: the local file name under `components/aic8800/firmware/`,
/// its path within the upstream repo, and the expected SHA-256 of its contents.
struct FirmwareFile {
    name: &'static str,
    remote_path: &'static str,
    sha256: &'static str,
}

/// The exact set of blobs referenced by `aic8800`'s `include_bytes!` calls.
/// Digests verified byte-for-byte against the pinned upstream commit.
const FIRMWARE_FILES: &[FirmwareFile] = &[
    FirmwareFile {
        name: "fmacfw.bin",
        remote_path: "aic8800_and_aic8800D80/fmacfw.bin",
        sha256: "2c6e70726df10ef74d9b1a657c74fdcfaeb88855b96b2c9bc8e0e603ac7c4cc3",
    },
    FirmwareFile {
        name: "fmacfw_patch.bin",
        remote_path: "aic8800_and_aic8800D80/fmacfw_patch.bin",
        sha256: "6c8126ad655e9971f05ca03dc60fa82cb6d48c3b02cf3ba960137566ce2e28d5",
    },
    FirmwareFile {
        name: "fmacfw_patch_8800dc_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_8800dc_u02.bin",
        sha256: "69d3ac2038da3b8e652ed1ec5079598ceb6df51db7b87b1d33f6d3c820c86a6f",
    },
    FirmwareFile {
        name: "fw_patch_8800dc_u02.bin",
        remote_path: "aic8800DC/fw_patch_8800dc_u02.bin",
        sha256: "c4087b95e788785df0fc55aa92152d214323ee028c70ba0ebb23944d4070340b",
    },
    FirmwareFile {
        name: "fw_patch_table_8800dc_u02.bin",
        remote_path: "aic8800DC/fw_patch_table_8800dc_u02.bin",
        sha256: "e7eea12cc85fca5d8667182b4520b6a0929044c70c6d9e9a3d7ece8b16169688",
    },
    FirmwareFile {
        name: "fmacfw_8800d80_u02.bin",
        remote_path: "aic8800_and_aic8800D80/fmacfw_8800d80_u02.bin",
        sha256: "ffb49ede6004e58453f01489edf28b888b509529c3173554c98aa94fbb33507d",
    },
    FirmwareFile {
        name: "fw_patch_8800d80_u02.bin",
        remote_path: "aic8800_and_aic8800D80/fw_patch_8800d80_u02.bin",
        sha256: "f0e2f5bbc17bc327ca7f1574ff55370dfd863d931514347bb4abc18a74f6218f",
    },
    FirmwareFile {
        name: "fw_patch_table_8800d80_u02.bin",
        remote_path: "aic8800_and_aic8800D80/fw_patch_table_8800d80_u02.bin",
        sha256: "9decb77435b7e9713e33e32da483d683b7329ed93b672b2d1b134031d7da5f67",
    },
];

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

    let client = reqwest::Client::new();
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
        let resp = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("failed to GET {url}"))?;
        if !resp.status().is_success() {
            bail!("GET {url} returned HTTP {}", resp.status());
        }
        let bytes = resp
            .bytes()
            .await
            .with_context(|| format!("failed to read body of {url}"))?;

        let actual = sha256_hex(&bytes);
        if actual != file.sha256 {
            bail!(
                "firmware {} sha256 mismatch: expected {}, got {} (from {url})",
                file.name,
                file.sha256,
                actual
            );
        }

        let dest = dir.join(file.name);
        std::fs::write(&dest, &bytes)
            .with_context(|| format!("failed to write {}", dest.display()))?;
        log::info!("  fetched {} ({} bytes)", file.name, bytes.len());
    }

    Ok(())
}
