//! Provision AIC8800 vendor firmware blobs into `OUT_DIR` at build time.
//!
//! `src/firmware/mod.rs` `include_bytes!`s the firmware from `OUT_DIR`, so
//! the blobs never need to live in the crate source / package tarball. This
//! keeps firmware out of the package while allowing a clean `cargo build` to
//! provision it without a workspace `cargo xtask` side effect. The first build
//! requires network access unless a verified local cache is supplied.
//!
//! Resolution order for each blob (first hit wins):
//!   1. `OUT_DIR/firmware/<name>` — a verified output from an earlier run.
//!   2. `$AIC8800_FIRMWARE_DIR/<name>` — explicit local cache / offline mirror.
//!   3. `drivers/net/aic8800/firmware/<name>` — optional in-tree cache for
//!      offline builds.
//!   4. download from the pinned upstream commit over HTTPS.
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

/// The exact set of blobs referenced by `src/firmware/mod.rs`.
const FIRMWARE_FILES: &[FirmwareFile] = &[
    FirmwareFile {
        name: "fmacfw_patch_8800dc_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_8800dc_u02.bin",
        sha256: "69d3ac2038da3b8e652ed1ec5079598ceb6df51db7b87b1d33f6d3c820c86a6f",
    },
    FirmwareFile {
        name: "fmacfw_patch_tbl_8800dc_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_tbl_8800dc_u02.bin",
        sha256: "62d53a223eda1ea064ba82a6fe67829d0720e9f4e87d26763fd13316ccd2a90b",
    },
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
    FirmwareFile {
        name: "fmacfw_calib_8800dc_u02.bin",
        remote_path: "aic8800DC/fmacfw_calib_8800dc_u02.bin",
        sha256: "db3c90ba2336f71b87f2e2b92e71b6b395422e146e64e6863013d553baa90b48",
    },
    FirmwareFile {
        name: "fmacfw_calib_8800dc_h_u02.bin",
        remote_path: "aic8800DC/fmacfw_calib_8800dc_h_u02.bin",
        sha256: "12bdcdd48e41b33bfd74834bffa326b4469bea82e7134de079392fbc2508acc7",
    },
    FirmwareFile {
        name: "fmacfw_patch_8800dc_hbt_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_8800dc_hbt_u02.bin",
        sha256: "d8cd9f2d4e7f6dafc1d221dbb1174bf21b64ae29b23efc118fc565872d184317",
    },
    FirmwareFile {
        name: "fmacfw_patch_tbl_8800dc_hbt_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_tbl_8800dc_hbt_u02.bin",
        sha256: "0ac8a2d85c86e3d9cb04cf5a27178c1d5bd6b1b4396c8a72af16b19b3e08c8d6",
    },
    FirmwareFile {
        name: "fmacfw_calib_8800dc_hbt_u02.bin",
        remote_path: "aic8800DC/fmacfw_calib_8800dc_hbt_u02.bin",
        sha256: "11e4cbf3985e5cd924a48774fd8a2a7c2c3fd0bdddcff4500d1d00d534af54d0",
    },
    FirmwareFile {
        name: "fmacfw_8800d80_u02.bin",
        remote_path: "aic8800_and_aic8800D80/fmacfw_8800d80_u02.bin",
        sha256: "ffb49ede6004e58453f01489edf28b888b509529c3173554c98aa94fbb33507d",
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
         drivers/net/aic8800/firmware/ — ensure it is checked out (it is allow-listed in that \
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
        let dest = fw_out.join(file.name);
        if read_if_matches(&dest, file.sha256).is_some() {
            continue;
        }

        // 2. explicit cache dir, 3. in-tree dir, else 4. download.
        let bytes = env_dir
            .as_ref()
            .and_then(|d| read_if_matches(&d.join(file.name), file.sha256))
            .or_else(|| read_if_matches(&in_tree.join(file.name), file.sha256))
            .unwrap_or_else(|| download(file));

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
