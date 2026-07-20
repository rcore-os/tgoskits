//! Provision AIC8800 vendor firmware blobs into `OUT_DIR` at build time.
//!
//! `src/firmware.rs` `include_bytes!`s the firmware from `OUT_DIR`, so
//! the blobs never need to live in the crate source / package tarball. This
//! keeps the published crate self-contained: a clean `cargo build` (e.g. when
//! verifying a `cargo publish` tarball) provisions the blobs here without
//! relying on the workspace `cargo xtask` pre-download side effect.
//!
//! Resolution order for each blob (first hit wins):
//!   1. `$AIC8800_FIRMWARE_DIR/<name>` — explicit local cache / offline mirror.
//!   2. `components/aic8800/firmware/<name>` — the in-tree dir that
//!      `cargo xtask` populates; used by normal in-repo (incl. offline) builds.
//!   3. download from the pinned upstream commit over HTTPS.
//!
//! Every blob is verified byte-for-byte against its pinned SHA-256 before being
//! copied into `OUT_DIR`, regardless of which source it came from.

use std::path::{Path, PathBuf};

#[path = "src/firmware_manifest.rs"]
mod firmware_manifest;

use firmware_manifest::{
    COMMIT as FIRMWARE_COMMIT, FILES as FIRMWARE_FILES, FirmwareFile, REPOSITORY as FIRMWARE_REPO,
};

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Read `path` and return its bytes if they match `expected_sha256`.
fn read_if_matches(path: &Path, expected_sha256: &str) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    (sha256_hex(&bytes) == expected_sha256).then_some(bytes)
}

/// Download a blob from the pinned upstream commit and verify its digest.
fn download(file: &FirmwareFile) -> Vec<u8> {
    assert!(
        !file.remote_path.is_empty(),
        "firmware {} has no upstream mirror (remote_path is empty) and was not found in the \
         in-tree firmware dir or $AIC8800_FIRMWARE_DIR. This blob is vendored in \
         components/aic8800/firmware/ — ensure it is checked out (it is allow-listed in that \
         dir's .gitignore).",
        file.name
    );
    let url = format!(
        "https://raw.githubusercontent.com/{}/{}/{}",
        FIRMWARE_REPO, FIRMWARE_COMMIT, file.remote_path
    );
    let mut resp = ureq::get(&url)
        .call()
        .unwrap_or_else(|e| panic!("failed to GET {url}: {e}"));
    let mut bytes = Vec::new();
    use std::io::Read;
    resp.body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .unwrap_or_else(|e| panic!("failed to read body of {url}: {e}"));
    let actual = sha256_hex(&bytes);
    assert!(
        actual == file.sha256,
        "firmware {} sha256 mismatch: expected {}, got {} (from {url})",
        file.name,
        file.sha256,
        actual
    );
    bytes
}

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    let fw_out = out_dir.join("firmware");
    std::fs::create_dir_all(&fw_out)
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", fw_out.display()));

    // Optional explicit cache / offline mirror.
    let env_dir = std::env::var("AIC8800_FIRMWARE_DIR")
        .ok()
        .map(PathBuf::from);
    // In-tree dir that `cargo xtask` populates (present for in-repo builds).
    let in_tree = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("firmware");

    for file in FIRMWARE_FILES {
        // 1. explicit cache dir, 2. in-tree dir, else 3. download.
        let bytes = env_dir
            .as_ref()
            .and_then(|d| read_if_matches(&d.join(file.name), file.sha256))
            .or_else(|| read_if_matches(&in_tree.join(file.name), file.sha256))
            .unwrap_or_else(|| download(file));

        let dest = fw_out.join(file.name);
        std::fs::write(&dest, &bytes)
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", dest.display()));
    }

    // Re-run only when the build script, the manifest, or the cache env changes.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=AIC8800_FIRMWARE_DIR");
    for file in FIRMWARE_FILES {
        println!("cargo:rerun-if-changed=firmware/{name}", name = file.name);
    }
}
