//! Provision AIC8800 vendor firmware blobs into `OUT_DIR` at build time.
//!
//! `src/fw/firmware/data.rs` `include_bytes!`s the firmware from `OUT_DIR`, so
//! the blobs never need to live in the crate source / package tarball. This
//! keeps the published crate self-contained: a clean `cargo build` (e.g. when
//! verifying a `cargo publish` tarball) provisions the blobs here without
//! relying on the workspace `cargo xtask` pre-download side effect.
//!
//! Resolution order for each blob (first hit wins):
//!   1. `$AIC8800_FIRMWARE_DIR/<name>` — explicit local cache / offline mirror.
//!   2. `components/aic8800/firmware/<name>` — optional in-tree cache for
//!      offline builds.
//!   3. download from the pinned upstream commit over HTTPS.
//!
//! Every blob is verified byte-for-byte against its pinned SHA-256 before being
//! copied into `OUT_DIR`, regardless of which source it came from.

use std::path::{Path, PathBuf};

/// Upstream firmware source: the repo referenced by the LicheeRV Nano
/// buildroot package `aic8800-sdio-firmware`, pinned to a fixed commit.
const FIRMWARE_REPO: &str = "lxowalle/aic8800-sdio-firmware";
const FIRMWARE_COMMIT: &str = "c56f910044cc854d6c553bcb9a644f3bca5a4c38";

struct FirmwareFile {
    /// File name under `OUT_DIR/firmware/` and the in-tree firmware dir.
    name: &'static str,
    /// Path within the upstream repo.
    remote_path: &'static str,
    /// Expected SHA-256 of the contents (lowercase hex).
    sha256: &'static str,
}

/// The exact set of blobs referenced by `src/fw/firmware/data.rs`.
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
        name: "fmacfw_patch_tbl_8800dc_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_tbl_8800dc_u02.bin",
        sha256: "62d53a223eda1ea064ba82a6fe67829d0720e9f4e87d26763fd13316ccd2a90b",
    },
    // AIC8800DC-H (sub_id==2, chip_id_h) WiFi-only patch + patch table.
    // Fetched from the pinned upstream mirror (same repo/commit as the other blobs).
    FirmwareFile {
        name: "fmacfw_patch_8800dc_h_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_8800dc_h_u02.bin",
        sha256: "f388dcb419a0f677c777a1eaad798156eabdfbb72c512a4d993df0dbc4f351d1",
    },
    FirmwareFile {
        name: "fmacfw_patch_tbl_8800dc_h_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_tbl_8800dc_h_u02.bin",
        sha256: "0469686691b72fa8296ff7abd1669ba978bdc0f115137fd392aa00a2717ff887",
    },
    // AIC8800DC-H DPD calibration firmware: uploaded to 0x130000 and run via
    // start_app(0x130009, FNCALL) to power on the RF/misc-RAM (0x110000) region
    // before patch_config. Fetched from the pinned upstream mirror.
    FirmwareFile {
        name: "fmacfw_calib_8800dc_h_u02.bin",
        remote_path: "aic8800DC/fmacfw_calib_8800dc_h_u02.bin",
        sha256: "12bdcdd48e41b33bfd74834bffa326b4469bea82e7134de079392fbc2508acc7",
    },
    // NB: the AIC8800DC RF config tables (ldpc/agc/txgain) are NOT firmware
    // images and have no upstream mirror — they are vendor BSP source arrays,
    // inlined as Rust byte arrays in `src/fw/firmware/dc_rf_cfg.rs` (no blob).
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
    // Optional in-tree cache for offline builds.
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
