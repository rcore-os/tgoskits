//! Verified OVMF firmware bundle handling for x86_64 AxVisor UEFI tests.
//!
//! A fixed OVMF build is distributed as a "bundle": a directory holding
//! `OVMF_CODE.fd`, `OVMF_VARS.fd`, `OVMF.fd` and a flat `manifest.toml`
//! describing the expected profile, GPA layout and per-file sizes and
//! SHA-256 digests. This module replaces the old
//! `os/axvisor/scripts/ovmf-profile.sh` verifier: it parses the manifest
//! with the typed `toml` crate and verifies every value and every file
//! against the fixed profile constants.
//!
//! Firmware that is not part of a managed bundle (for example a local
//! `OVMF_CODE.fd` supplied by the developer) is only accepted through an
//! explicit opt-in; it must still match the fixed CODE size and its SHA-256
//! digest is printed so the caller can see exactly what was used.
//!
//! Verification never downloads anything. If a bundle directory is missing,
//! the caller is expected to pull it first through the image registry; this
//! module only validates a bundle that is already present on disk.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::support::download::file_sha256;

/// Fixed x86_64 guest firmware profile name, also the image registry entry
/// name under which the managed bundle is published.
pub const OVMF_PROFILE_NAME: &str = "qemu_x86_64_axvisor_ovmf_debug";

/// EDK2 tag the fixed bundle was built from.
pub const OVMF_EDK2_TAG: &str = "edk2-stable202605";

/// EDK2 commit the fixed bundle was built from.
pub const OVMF_EDK2_COMMIT: &str = "b03a21a63e3bd001f52c527e5a57feddb53a690b";

/// Guest physical address of the start of the CODE (firmware text) region.
pub const OVMF_CODE_BASE: u64 = 0xffc8_4000;

/// Size of the CODE image in bytes.
pub const OVMF_CODE_SIZE: u64 = 0x37_c000;

/// Guest physical address of the start of the VARS (NVRAM) region.
pub const OVMF_VARS_BASE: u64 = 0xffc0_0000;

/// Size of the VARS image in bytes.
pub const OVMF_VARS_SIZE: u64 = 0x8_4000;

/// Size of the combined `OVMF.fd` image in bytes (VARS followed by CODE).
pub const OVMF_COMBINED_SIZE: u64 = 0x40_0000;

/// Guest physical address of the x86 reset vector, expected to sit inside
/// the CODE window.
pub const OVMF_RESET_VECTOR: u64 = 0xffff_fff0;

/// CODE image file name inside a managed bundle.
pub const CODE_FILE: &str = "OVMF_CODE.fd";

/// VARS image file name inside a managed bundle.
pub const VARS_FILE: &str = "OVMF_VARS.fd";

/// Combined image file name inside a managed bundle.
pub const COMBINED_FILE: &str = "OVMF.fd";

/// Flat manifest file name inside a managed bundle.
pub const MANIFEST_FILE: &str = "manifest.toml";

/// Where a firmware image came from.
///
/// The enum keeps the "verified managed bundle" and "explicit unverified
/// local file" cases distinct so callers cannot accidentally mix the two:
/// the two paths carry different semantics (manifest verification vs. a
/// printed digest) and different CLI requirements (opt-in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirmwareSource {
    /// A managed, manifest-verified OVMF bundle directory.
    Managed { bundle_dir: PathBuf },
    /// A local firmware file accepted only through an explicit opt-in.
    UnverifiedLocal { code_path: PathBuf },
}

impl FirmwareSource {
    /// Construct a [`FirmwareSource`] from a CLI-provided bundle path.
    ///
    /// `allow_unverified` is the CLI opt-in flag; without it a source that
    /// does not look like a managed bundle is rejected.
    pub fn from_cli(
        firmware_bundle_path: Option<PathBuf>,
        allow_unverified: bool,
    ) -> Result<Option<Self>> {
        let Some(path) = firmware_bundle_path else {
            return Ok(None);
        };
        let path = resolve_firmware_path(&path)?;
        if path.is_dir() {
            if !path.join(MANIFEST_FILE).is_file() {
                bail!(
                    "firmware bundle {} is missing {}; pull the verified image first",
                    path.display(),
                    MANIFEST_FILE
                );
            }
            return Ok(Some(Self::Managed { bundle_dir: path }));
        }
        if !allow_unverified {
            bail!(
                "firmware path {} is not a managed bundle directory; pass \
                 --allow-unverified-firmware to use an unverified local firmware file",
                path.display()
            );
        }
        Ok(Some(Self::UnverifiedLocal { code_path: path }))
    }
}

/// Fully validated result of verifying a firmware source.
///
/// The fields are the pieces later tasks need (loader GPA layout, file
/// paths, digest, verification label); the struct is immutable once built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOvmfBundle {
    /// Firmware profile name the bundle claims (or the fixed profile for
    /// unverified local firmware).
    pub profile: String,
    /// Guest physical address of the CODE window start.
    pub code_base: u64,
    /// CODE image size in bytes.
    pub code_size: u64,
    /// Guest physical address of the VARS window start.
    pub vars_base: u64,
    /// VARS image size in bytes.
    pub vars_size: u64,
    /// Combined image size in bytes.
    pub combined_size: u64,
    /// Path of the CODE image to load.
    pub code_path: PathBuf,
    /// SHA-256 of the CODE image.
    pub code_sha256: String,
    /// `true` when the bundle passed full manifest verification.
    pub verified: bool,
}

impl VerifiedOvmfBundle {
    /// Guest physical address one past the end of the CODE window.
    pub fn code_end(&self) -> u64 {
        self.code_base + self.code_size
    }

    /// True when the reset vector address lies within the CODE window.
    pub fn reset_vector_in_code_window(&self) -> bool {
        OVMF_RESET_VECTOR >= self.code_base && OVMF_RESET_VECTOR + 16 <= self.code_end()
    }
}

/// Verify the firmware selected on the command line.
///
/// Managed bundles are fully verified against `manifest.toml`; unverified
/// local firmware is checked for the fixed CODE size only and its SHA-256
/// is printed.
pub fn verify_firmware(source: &FirmwareSource) -> Result<VerifiedOvmfBundle> {
    match source {
        FirmwareSource::Managed { bundle_dir } => verify_managed_bundle(bundle_dir),
        FirmwareSource::UnverifiedLocal { code_path } => {
            verify_unverified_local_firmware(code_path)
        }
    }
}

fn resolve_firmware_path(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("firmware path {} does not exist", path.display())
        }
        Err(err) => {
            Err(err).with_context(|| format!("failed to resolve firmware path {}", path.display()))
        }
    }
}

fn verify_managed_bundle(bundle_dir: &Path) -> Result<VerifiedOvmfBundle> {
    let manifest_path = bundle_dir.join(MANIFEST_FILE);
    let manifest = fs::read_to_string(&manifest_path).with_context(|| {
        format!(
            "failed to read OVMF bundle manifest {}",
            manifest_path.display()
        )
    })?;
    let manifest = parse_manifest(&manifest, &manifest_path)?;
    verify_manifest(&manifest, bundle_dir)
}

fn parse_manifest(contents: &str, path: &Path) -> Result<OvmfManifest> {
    toml::from_str::<OvmfManifest>(contents).with_context(|| {
        format!(
            "failed to parse OVMF bundle manifest {} (expected a flat, single-table manifest)",
            path.display()
        )
    })
}

fn verify_manifest(manifest: &OvmfManifest, bundle_dir: &Path) -> Result<VerifiedOvmfBundle> {
    let manifest_label = bundle_dir.join(MANIFEST_FILE).display().to_string();

    require_eq(
        "schema_version",
        manifest.schema_version,
        1,
        &manifest_label,
    )?;
    require_str_eq(
        "profile",
        &manifest.profile,
        OVMF_PROFILE_NAME,
        &manifest_label,
    )?;
    require_str_eq(
        "edk2_tag",
        &manifest.edk2_tag,
        OVMF_EDK2_TAG,
        &manifest_label,
    )?;
    require_str_eq(
        "edk2_commit",
        &manifest.edk2_commit,
        OVMF_EDK2_COMMIT,
        &manifest_label,
    )?;
    require_str_eq(
        "architecture",
        &manifest.architecture,
        "X64",
        &manifest_label,
    )?;
    require_str_eq("target", &manifest.target, "DEBUG", &manifest_label)?;
    require_str_eq("toolchain", &manifest.toolchain, "GCC", &manifest_label)?;
    require_str_eq(
        "platform",
        &manifest.platform,
        "OvmfPkg/OvmfPkgX64.dsc",
        &manifest_label,
    )?;
    require_num_eq(
        "code_base",
        manifest.code_base,
        OVMF_CODE_BASE,
        &manifest_label,
    )?;
    require_num_eq(
        "code_size",
        manifest.code_size,
        OVMF_CODE_SIZE,
        &manifest_label,
    )?;
    require_num_eq(
        "vars_base",
        manifest.vars_base,
        OVMF_VARS_BASE,
        &manifest_label,
    )?;
    require_num_eq(
        "vars_size",
        manifest.vars_size,
        OVMF_VARS_SIZE,
        &manifest_label,
    )?;
    require_num_eq(
        "combined_size",
        manifest.combined_size,
        OVMF_COMBINED_SIZE,
        &manifest_label,
    )?;
    require_num_eq(
        "reset_vector",
        manifest.reset_vector,
        OVMF_RESET_VECTOR,
        &manifest_label,
    )?;
    require_str_eq("code_file", &manifest.code_file, CODE_FILE, &manifest_label)?;
    require_str_eq("vars_file", &manifest.vars_file, VARS_FILE, &manifest_label)?;
    require_str_eq(
        "combined_file",
        &manifest.combined_file,
        COMBINED_FILE,
        &manifest_label,
    )?;
    require_bool_eq("fd_size_4mb", manifest.fd_size_4mb, true, &manifest_label)?;
    require_bool_eq(
        "debug_on_serial_port",
        manifest.debug_on_serial_port,
        true,
        &manifest_label,
    )?;
    require_bool_eq("build_shell", manifest.build_shell, true, &manifest_label)?;
    require_bool_eq("smm_require", manifest.smm_require, false, &manifest_label)?;
    require_bool_eq(
        "secure_boot_enable",
        manifest.secure_boot_enable,
        false,
        &manifest_label,
    )?;
    require_bool_eq("tpm2_enable", manifest.tpm2_enable, false, &manifest_label)?;
    require_bool_eq(
        "network_enable",
        manifest.network_enable,
        false,
        &manifest_label,
    )?;
    require_bool_eq(
        "sdcard_enable",
        manifest.sdcard_enable,
        false,
        &manifest_label,
    )?;
    require_bool_eq(
        "cc_measurement_enable",
        manifest.cc_measurement_enable,
        false,
        &manifest_label,
    )?;
    require_str_eq(
        "sec_marker",
        &manifest.sec_marker,
        "SecCoreStartupWithStack(",
        &manifest_label,
    )?;
    require_str_eq(
        "pei_marker",
        &manifest.pei_marker,
        "Platform PEIM Loaded",
        &manifest_label,
    )?;
    require_str_eq(
        "dxe_ipl_marker",
        &manifest.dxe_ipl_marker,
        "DXE IPL Entry",
        &manifest_label,
    )?;
    require_str_eq(
        "dxe_core_marker",
        &manifest.dxe_core_marker,
        "Loading DXE CORE at",
        &manifest_label,
    )?;
    require_str_eq(
        "bds_marker",
        &manifest.bds_marker,
        "[BdsDxe]",
        &manifest_label,
    )?;
    require_non_empty("build_command", &manifest.build_command, &manifest_label)?;
    require_non_empty(
        "build_container_digest",
        &manifest.build_container_digest,
        &manifest_label,
    )?;
    require_non_empty("tool_versions", &manifest.tool_versions, &manifest_label)?;
    require_non_empty(
        "submodule_commits",
        &manifest.submodule_commits,
        &manifest_label,
    )?;

    let code_path = bundle_dir.join(CODE_FILE);
    let vars_path = bundle_dir.join(VARS_FILE);
    let combined_path = bundle_dir.join(COMBINED_FILE);

    let code_sha256 = verify_manifest_file(
        &code_path,
        manifest.code_sha256.as_deref(),
        manifest.code_size,
        "code",
        OVMF_CODE_SIZE,
    )?;
    let vars_sha256 = verify_manifest_file(
        &vars_path,
        manifest.vars_sha256.as_deref(),
        manifest.vars_size,
        "vars",
        OVMF_VARS_SIZE,
    )?;
    let combined_sha256 = verify_manifest_file(
        &combined_path,
        manifest.combined_sha256.as_deref(),
        manifest.combined_size,
        "combined",
        OVMF_COMBINED_SIZE,
    )?;

    let vars =
        fs::read(&vars_path).with_context(|| format!("failed to read {}", vars_path.display()))?;
    let code =
        fs::read(&code_path).with_context(|| format!("failed to read {}", code_path.display()))?;
    let mut expected_combined = Vec::with_capacity(vars.len() + code.len());
    expected_combined.extend_from_slice(&vars);
    expected_combined.extend_from_slice(&code);
    let combined = fs::read(&combined_path)
        .with_context(|| format!("failed to read {}", combined_path.display()))?;
    if combined != expected_combined {
        bail!(
            "{} is not the byte-for-byte concatenation of {} and {} (in that order)",
            combined_path.display(),
            VARS_FILE,
            CODE_FILE
        );
    }

    let code_end = manifest
        .code_base
        .checked_add(manifest.code_size)
        .ok_or_else(|| anyhow!("code_base + code_size overflows u64"))?;
    let reset_vector_end = manifest
        .reset_vector
        .checked_add(16)
        .ok_or_else(|| anyhow!("reset_vector + 16 overflows u64"))?;
    if manifest.reset_vector < manifest.code_base || reset_vector_end > code_end {
        bail!(
            "reset vector 0x{OVMF_RESET_VECTOR:x} is outside the CODE window [0x{:x}, 0x{:x})",
            manifest.code_base,
            code_end
        );
    }

    let bundle = VerifiedOvmfBundle {
        profile: manifest.profile.clone(),
        code_base: manifest.code_base,
        code_size: manifest.code_size,
        vars_base: manifest.vars_base,
        vars_size: manifest.vars_size,
        combined_size: manifest.combined_size,
        code_path,
        code_sha256,
        verified: true,
    };
    print_selected_firmware(&bundle, Some(vars_sha256), Some(combined_sha256));
    Ok(bundle)
}

fn verify_manifest_file(
    path: &Path,
    expected_sha256: Option<&str>,
    manifest_size: u64,
    role: &str,
    expected_size: u64,
) -> Result<String> {
    let Some(expected_sha256) = expected_sha256 else {
        bail!(
            "OVMF manifest is missing {role}_sha256 (file {})",
            path.display()
        );
    };
    if !is_sha256_hex(expected_sha256) {
        bail!("OVMF manifest has an invalid {role}_sha256: {expected_sha256}");
    }
    let actual_size = fs::metadata(path)
        .with_context(|| format!("failed to read metadata of {}", path.display()))?
        .len();
    if actual_size != expected_size {
        bail!(
            "{} has size {}; expected {} (manifest declares {})",
            path.display(),
            actual_size,
            expected_size,
            manifest_size
        );
    }
    let actual_sha256 = file_sha256(path)?;
    if actual_sha256 != expected_sha256 {
        bail!(
            "SHA-256 mismatch for {}: expected {}, got {}",
            path.display(),
            expected_sha256,
            actual_sha256
        );
    }
    Ok(actual_sha256)
}

fn verify_unverified_local_firmware(code_path: &Path) -> Result<VerifiedOvmfBundle> {
    if !code_path.is_file() {
        bail!(
            "unverified UEFI firmware does not exist: {}",
            code_path.display()
        );
    }
    let actual_size = fs::metadata(code_path)
        .with_context(|| format!("failed to read metadata of {}", code_path.display()))?
        .len();
    if actual_size != OVMF_CODE_SIZE {
        bail!(
            "unverified UEFI firmware must still match the fixed CODE size {} bytes; got {}",
            OVMF_CODE_SIZE,
            actual_size
        );
    }
    let code_sha256 = file_sha256(code_path)?;
    println!(
        "unverified firmware: path={}, size={}, sha256={}",
        code_path.display(),
        actual_size,
        code_sha256
    );
    let bundle = VerifiedOvmfBundle {
        profile: OVMF_PROFILE_NAME.to_string(),
        code_base: OVMF_CODE_BASE,
        code_size: OVMF_CODE_SIZE,
        vars_base: OVMF_VARS_BASE,
        vars_size: OVMF_VARS_SIZE,
        combined_size: OVMF_COMBINED_SIZE,
        code_path: code_path.to_path_buf(),
        code_sha256,
        verified: false,
    };
    print_selected_firmware(&bundle, None, None);
    eprintln!(
        "WARNING: UNVERIFIED firmware is diagnostic-only and must not determine UEFI test results."
    );
    Ok(bundle)
}

fn print_selected_firmware(
    bundle: &VerifiedOvmfBundle,
    vars_sha256: Option<String>,
    combined_sha256: Option<String>,
) {
    println!("selected firmware profile: {}", bundle.profile);
    println!("  code: path={}", bundle.code_path.display());
    println!(
        "  code: gpa=0x{:x}, size=0x{:x}",
        bundle.code_base, bundle.code_size
    );
    println!(
        "  vars: gpa=0x{:x}, size=0x{:x}",
        bundle.vars_base, bundle.vars_size
    );
    println!("  combined: size=0x{:x}", bundle.combined_size);
    println!("  code_sha256={}", bundle.code_sha256);
    if let Some(vars_sha256) = vars_sha256 {
        println!("  vars_sha256={vars_sha256}");
    }
    if let Some(combined_sha256) = combined_sha256 {
        println!("  combined_sha256={combined_sha256}");
    }
    println!(
        "  verification_label={}",
        if bundle.verified {
            "VERIFIED"
        } else {
            "UNVERIFIED"
        }
    );
}

fn require_str_eq(field: &str, actual: &str, expected: &str, where_: &str) -> Result<()> {
    if actual != expected {
        bail!("OVMF manifest {where_} {field}={actual:?}; expected {expected:?}");
    }
    Ok(())
}

fn require_num_eq(field: &str, actual: u64, expected: u64, where_: &str) -> Result<()> {
    if actual != expected {
        bail!("OVMF manifest {where_} {field}=0x{actual:x}; expected 0x{expected:x}");
    }
    Ok(())
}

fn require_eq(field: &str, actual: u64, expected: u64, where_: &str) -> Result<()> {
    if actual != expected {
        bail!("OVMF manifest {where_} {field}={actual}; expected {expected}");
    }
    Ok(())
}

fn require_bool_eq(field: &str, actual: bool, expected: bool, where_: &str) -> Result<()> {
    if actual != expected {
        bail!("OVMF manifest {where_} {field}={actual}; expected {expected}");
    }
    Ok(())
}

fn require_non_empty(field: &str, actual: &str, where_: &str) -> Result<()> {
    if actual.is_empty() {
        bail!("OVMF manifest {where_} {field} must not be empty");
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Flat OVMF bundle manifest. Parsed with `deny_unknown_fields` so a
/// manifest drifting from the fixed contract is rejected instead of being
/// silently accepted with ignored fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OvmfManifest {
    schema_version: u64,
    profile: String,
    edk2_tag: String,
    edk2_commit: String,
    architecture: String,
    target: String,
    toolchain: String,
    platform: String,
    build_command: String,
    build_container_digest: String,
    tool_versions: String,
    submodule_commits: String,
    code_base: u64,
    code_size: u64,
    vars_base: u64,
    vars_size: u64,
    combined_size: u64,
    reset_vector: u64,
    code_file: String,
    code_sha256: Option<String>,
    vars_file: String,
    vars_sha256: Option<String>,
    combined_file: String,
    combined_sha256: Option<String>,
    fd_size_4mb: bool,
    debug_on_serial_port: bool,
    build_shell: bool,
    smm_require: bool,
    secure_boot_enable: bool,
    tpm2_enable: bool,
    network_enable: bool,
    sdcard_enable: bool,
    cc_measurement_enable: bool,
    sec_marker: String,
    pei_marker: String,
    dxe_ipl_marker: String,
    dxe_core_marker: String,
    bds_marker: String,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::*;

    const FIXED_CODE_BYTES: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];
    const FIXED_VARS_BYTES: [u8; 3] = [0xfe, 0xed, 0xfa];

    #[test]
    fn verifies_a_valid_managed_bundle() {
        let dir = tempdir().unwrap();
        let (code, vars, combined) = write_bundle(dir.path(), &bundle_manifest_string);

        let bundle = verify_firmware(&FirmwareSource::Managed {
            bundle_dir: dir.path().to_path_buf(),
        })
        .unwrap();

        assert_eq!(bundle.profile, OVMF_PROFILE_NAME);
        assert_eq!(bundle.code_base, OVMF_CODE_BASE);
        assert_eq!(bundle.code_size, OVMF_CODE_SIZE);
        assert_eq!(bundle.vars_base, OVMF_VARS_BASE);
        assert_eq!(bundle.vars_size, OVMF_VARS_SIZE);
        assert_eq!(bundle.combined_size, OVMF_COMBINED_SIZE);
        assert_eq!(bundle.code_path, dir.path().join(CODE_FILE));
        assert_eq!(bundle.code_sha256, sha256(&code));
        assert!(bundle.verified);
        assert!(bundle.reset_vector_in_code_window());
        assert_eq!(bundle.code_end(), OVMF_CODE_BASE + OVMF_CODE_SIZE);
        assert_eq!(vars.len(), OVMF_VARS_SIZE as usize);
        assert_eq!(combined.len(), OVMF_COMBINED_SIZE as usize);
    }

    #[test]
    fn rejects_wrong_code_sha256() {
        let dir = tempdir().unwrap();
        let (code, vars, combined) = write_bundle(dir.path(), &bundle_manifest_string);
        let wrong = wrong_hash(&code);
        fs::write(dir.path().join(CODE_FILE), &code).unwrap();
        fs::write(
            dir.path().join(MANIFEST_FILE),
            bundle_manifest_string(&code, &vars, &combined).replace(&sha256(&code), &wrong),
        )
        .unwrap();

        let err = verify_err(dir.path());
        assert!(
            err.contains("SHA-256 mismatch")
                && err.contains(&wrong)
                && err.contains(&sha256(&code)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_wrong_code_size() {
        let dir = tempdir().unwrap();
        let (code, vars, combined) = write_bundle(dir.path(), &bundle_manifest_string);
        let mut short_code = code.clone();
        short_code.pop();
        fs::write(dir.path().join(CODE_FILE), short_code).unwrap();
        fs::write(
            dir.path().join(MANIFEST_FILE),
            bundle_manifest_string(&code, &vars, &combined),
        )
        .unwrap();

        let err = verify_err(dir.path());
        assert!(
            err.contains("has size") && err.contains("expected"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_wrong_code_base() {
        let dir = tempdir().unwrap();
        let (code, vars, combined) = write_bundle(dir.path(), &bundle_manifest_string);
        let manifest = bundle_manifest_string(&code, &vars, &combined)
            .replace("code_base = 0xffc84000", "code_base = 0xffc00000");
        fs::write(dir.path().join(MANIFEST_FILE), manifest).unwrap();

        let err = verify_err(dir.path());
        assert!(
            err.contains("code_base") && err.contains("0xffc84000") && err.contains("0xffc00000"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_wrong_profile_name() {
        let dir = tempdir().unwrap();
        let (code, vars, combined) = write_bundle(dir.path(), &bundle_manifest_string);
        let manifest = bundle_manifest_string(&code, &vars, &combined)
            .replace(OVMF_PROFILE_NAME, "some-other-profile");
        fs::write(dir.path().join(MANIFEST_FILE), manifest).unwrap();

        let err = verify_err(dir.path());
        assert!(
            err.contains("profile") && err.contains("some-other-profile"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_unknown_manifest_field() {
        let dir = tempdir().unwrap();
        let (code, vars, combined) = write_bundle(dir.path(), &bundle_manifest_string);
        let manifest = format!(
            "{}\nunknown_field = \"anything\"\n",
            bundle_manifest_string(&code, &vars, &combined)
        );
        fs::write(dir.path().join(MANIFEST_FILE), manifest).unwrap();

        let err = verify_err(dir.path());
        assert!(err.contains("unknown_field"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_duplicate_manifest_field() {
        for duplicate in [
            "profile = \"ignored-duplicate\"",
            "code_base = 0xffc00000",
            "code_sha256 = \"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\"",
        ] {
            let dir = tempdir().unwrap();
            let (code, vars, combined) = write_bundle(dir.path(), &bundle_manifest_string);
            let manifest = format!(
                "{}\n{duplicate}\n",
                bundle_manifest_string(&code, &vars, &combined)
            );
            fs::write(dir.path().join(MANIFEST_FILE), manifest).unwrap();

            let err = verify_err(dir.path());
            assert!(
                err.contains("duplicate"),
                "accepted duplicate key {duplicate:?}, error: {err}"
            );
        }
    }

    #[test]
    fn rejects_wrong_combined_file_layout() {
        let dir = tempdir().unwrap();
        let (code, vars, _combined) = write_bundle(dir.path(), &bundle_manifest_string);
        let mut swapped = Vec::with_capacity(code.len() + vars.len());
        swapped.extend_from_slice(&code);
        swapped.extend_from_slice(&vars);
        fs::write(dir.path().join(COMBINED_FILE), &swapped).unwrap();
        fs::write(
            dir.path().join(MANIFEST_FILE),
            bundle_manifest_string(&code, &vars, &swapped),
        )
        .unwrap();

        let err = verify_err(dir.path());
        assert!(
            err.contains("byte-for-byte concatenation"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_wrong_reset_vector_value() {
        let dir = tempdir().unwrap();
        let (code, vars, combined) = write_bundle(dir.path(), &bundle_manifest_string);
        let manifest = bundle_manifest_string(&code, &vars, &combined)
            .replace("reset_vector = 0xfffffff0", "reset_vector = 0xffff0000");
        fs::write(dir.path().join(MANIFEST_FILE), manifest).unwrap();

        let err = verify_err(dir.path());
        assert!(
            err.contains("reset_vector") && err.contains("0xfffffff0"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn reset_vector_window_predicate_rejects_windows_outside_4gib() {
        // The fixed profile puts the reset vector at the very end of the
        // 4 GiB window, so the predicate holds for the verified bundle and
        // rejects any smaller CODE window.
        let mut bundle = verified_fixture_bundle(Path::new("OVMF_CODE.fd"));
        assert!(bundle.reset_vector_in_code_window());

        bundle.code_base = OVMF_VARS_BASE;
        bundle.code_size = OVMF_VARS_SIZE;
        assert!(!bundle.reset_vector_in_code_window());
    }

    #[test]
    fn rejects_missing_sha256_field() {
        let dir = tempdir().unwrap();
        let (code, vars, combined) = write_bundle(dir.path(), &bundle_manifest_string);
        let manifest = bundle_manifest_string(&code, &vars, &combined)
            .lines()
            .filter(|line| !line.starts_with("code_sha256"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.path().join(MANIFEST_FILE), manifest).unwrap();

        let err = verify_err(dir.path());
        assert!(
            err.contains("missing code_sha256"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_unverified_firmware_without_opt_in() {
        let dir = tempdir().unwrap();
        let code_path = dir.path().join("local-OVMF_CODE.fd");
        fs::write(&code_path, vec![0xa5; OVMF_CODE_SIZE as usize]).unwrap();

        let source = FirmwareSource::from_cli(Some(code_path.clone()), false);

        assert!(source.is_err());
        let err = format!("{:?}", source.err().unwrap().to_string());
        assert!(
            err.contains("--allow-unverified-firmware"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn accepts_unverified_firmware_with_opt_in_and_prints_sha256() {
        let dir = tempdir().unwrap();
        let code_path = dir.path().join("local-OVMF_CODE.fd");
        let code = vec![0xa5; OVMF_CODE_SIZE as usize];
        fs::write(&code_path, &code).unwrap();

        let source = FirmwareSource::from_cli(Some(code_path.clone()), true).unwrap();
        let source = source.unwrap();

        assert_eq!(
            source,
            FirmwareSource::UnverifiedLocal {
                code_path: code_path.clone()
            }
        );

        let bundle = verify_firmware(&source).unwrap();
        assert_eq!(bundle.code_path, code_path);
        assert_eq!(bundle.code_sha256, sha256(&code));
        assert_eq!(bundle.code_size, OVMF_CODE_SIZE);
        assert!(!bundle.verified);
        assert!(bundle.reset_vector_in_code_window());
    }

    #[test]
    fn rejects_unverified_firmware_with_wrong_size_even_with_opt_in() {
        let dir = tempdir().unwrap();
        let code_path = dir.path().join("local-OVMF_CODE.fd");
        fs::write(&code_path, vec![0xa5; 0x1000]).unwrap();

        let source = FirmwareSource::from_cli(Some(code_path.clone()), true).unwrap();
        let source = source.unwrap();
        let err = format!("{:?}", verify_firmware(&source).unwrap_err().to_string());

        assert!(
            err.contains("must still match the fixed CODE size"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn managed_bundle_requires_manifest_present() {
        let dir = tempdir().unwrap();
        write_bundle(dir.path(), &bundle_manifest_string);
        fs::remove_file(dir.path().join(MANIFEST_FILE)).unwrap();

        let err = format!(
            "{:?}",
            FirmwareSource::from_cli(Some(dir.path().to_path_buf()), false)
                .unwrap_err()
                .to_string()
        );

        assert!(
            err.contains("missing manifest.toml"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_nonexistent_firmware_path() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");

        let err = format!(
            "{:?}",
            FirmwareSource::from_cli(Some(missing.clone()), true)
                .unwrap_err()
                .to_string()
        );

        assert!(err.contains("does not exist"), "unexpected error: {err}");
    }

    #[test]
    fn from_cli_none_is_accepted_without_flags() {
        assert_eq!(FirmwareSource::from_cli(None, false).unwrap(), None);
    }

    fn verify_err(bundle_dir: &Path) -> String {
        format!(
            "{:#}",
            verify_firmware(&FirmwareSource::Managed {
                bundle_dir: bundle_dir.to_path_buf()
            })
            .unwrap_err()
        )
    }

    /// Writes the flat manifest for a fixed bundle from the three images.
    type ManifestWriter = dyn Fn(&[u8], &[u8], &[u8]) -> String;

    /// Write a complete managed bundle (CODE, VARS, combined image and flat
    /// manifest) with fixed byte patterns, returning the three images.
    fn write_bundle(bundle_dir: &Path, manifest: &ManifestWriter) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut code = FIXED_CODE_BYTES.to_vec();
        code.resize(OVMF_CODE_SIZE as usize, 0xa5);
        let mut vars = FIXED_VARS_BYTES.to_vec();
        vars.resize(OVMF_VARS_SIZE as usize, 0x5a);
        let mut combined = Vec::with_capacity(vars.len() + code.len());
        combined.extend_from_slice(&vars);
        combined.extend_from_slice(&code);

        fs::write(bundle_dir.join(CODE_FILE), &code).unwrap();
        fs::write(bundle_dir.join(VARS_FILE), &vars).unwrap();
        fs::write(bundle_dir.join(COMBINED_FILE), &combined).unwrap();
        fs::write(
            bundle_dir.join(MANIFEST_FILE),
            manifest(&code, &vars, &combined),
        )
        .unwrap();
        (code, vars, combined)
    }

    fn bundle_manifest_string(code: &[u8], vars: &[u8], combined: &[u8]) -> String {
        format!(
            r#"schema_version = 1
profile = "qemu_x86_64_axvisor_ovmf_debug"
edk2_tag = "edk2-stable202605"
edk2_commit = "b03a21a63e3bd001f52c527e5a57feddb53a690b"
architecture = "X64"
target = "DEBUG"
toolchain = "GCC"
platform = "OvmfPkg/OvmfPkgX64.dsc"
build_command = "build -a X64 -b DEBUG"
build_container_digest = "sha256:fixture"
tool_versions = "fixture"
submodule_commits = "fixture"
code_base = 0xffc84000
code_size = 0x37c000
vars_base = 0xffc00000
vars_size = 0x84000
combined_size = 0x400000
reset_vector = 0xfffffff0
code_file = "OVMF_CODE.fd"
code_sha256 = "{code_sha256}"
vars_file = "OVMF_VARS.fd"
vars_sha256 = "{vars_sha256}"
combined_file = "OVMF.fd"
combined_sha256 = "{combined_sha256}"
fd_size_4mb = true
debug_on_serial_port = true
build_shell = true
smm_require = false
secure_boot_enable = false
tpm2_enable = false
network_enable = false
sdcard_enable = false
cc_measurement_enable = false
sec_marker = "SecCoreStartupWithStack("
pei_marker = "Platform PEIM Loaded"
dxe_ipl_marker = "DXE IPL Entry"
dxe_core_marker = "Loading DXE CORE at"
bds_marker = "[BdsDxe]"
"#,
            code_sha256 = sha256(code),
            vars_sha256 = sha256(vars),
            combined_sha256 = sha256(combined),
        )
    }

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn wrong_hash(bytes: &[u8]) -> String {
        let mut hash = sha256(bytes).into_bytes();
        let digit = hash.first_mut().unwrap();
        *digit = if *digit == b'0' { b'1' } else { b'0' };
        String::from_utf8(hash).unwrap()
    }

    /// Build a [`VerifiedOvmfBundle`] with the fixed profile layout for
    /// testing predicates that do not require real files on disk.
    fn verified_fixture_bundle(code_path: &Path) -> VerifiedOvmfBundle {
        VerifiedOvmfBundle {
            profile: OVMF_PROFILE_NAME.to_string(),
            code_base: OVMF_CODE_BASE,
            code_size: OVMF_CODE_SIZE,
            vars_base: OVMF_VARS_BASE,
            vars_size: OVMF_VARS_SIZE,
            combined_size: OVMF_COMBINED_SIZE,
            code_path: code_path.to_path_buf(),
            code_sha256: sha256(&FIXED_CODE_BYTES),
            verified: true,
        }
    }
}
