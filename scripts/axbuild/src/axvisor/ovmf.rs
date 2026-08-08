//! Verified OVMF firmware handling for x86_64 AxVisor UEFI tests.
//!
//! The ovmf-entry case boots the upstream RELEASE OVMF built by Ostool
//! (`edk2-stable202508-r1`). The firmware is fetched through the Ostool
//! `Source::LATEST` cache (a directory holding `code.fd` and `vars.fd`), whose
//! SHA-256 verification is done by Ostool itself; this module validates the
//! layout of that firmware against the fixed x86_64 profile constants. The
//! layout matches the checked-in `qemu_x86_64_axvisor_ovmf_debug` profile
//! (code at 0xffc8_4000, vars at 0xffc0_0000, combined 4 MiB, reset vector at
//! 0xffff_fff0), which the x86_64 guest loader still enforces.
//!
//! Firmware that is not part of the Ostool cache (for example a local
//! `code.fd` supplied by the developer) is only accepted through an explicit
//! opt-in; it must still match the fixed CODE size and its SHA-256 digest is
//! printed so the caller can see exactly what was used.
//!
//! Verification never downloads anything. If the firmware directory is
//! missing, the caller is expected to prepare the Ostool cache first (for
//! example through `cargo xtask ovmf`); this module only validates firmware
//! that is already present on disk.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::support::download::file_sha256;

/// File name of the upstream RELEASE code image inside an Ostool firmware
/// directory.
pub const UPSTREAM_CODE_FILE: &str = "code.fd";

/// File name of the upstream RELEASE variable store inside an Ostool firmware
/// directory.
pub const UPSTREAM_VARS_FILE: &str = "vars.fd";

/// Fixed x86_64 guest firmware profile name, matching the profile enforced by
/// the x86_64 guest loader (`FIXED_OVMF_PROFILE` in `axvm`) and written into
/// the generated ovmf-entry VM config. The upstream RELEASE layout matches
/// this profile exactly.
pub const OVMF_PROFILE_NAME: &str = "qemu_x86_64_axvisor_ovmf_debug";

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

/// Where a firmware image came from.
///
/// The enum keeps the "upstream RELEASE firmware" and "explicit unverified
/// local file" cases distinct so callers cannot accidentally mix the two: the
/// two paths carry different semantics (layout verification vs. a printed
/// digest) and different CLI requirements (opt-in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirmwareSource {
    /// An upstream RELEASE firmware directory holding `code.fd` and
    /// `vars.fd`, as produced by the Ostool cache.
    Upstream {
        code_path: PathBuf,
        vars_path: PathBuf,
    },
    /// A local firmware file accepted only through an explicit opt-in.
    UnverifiedLocal { code_path: PathBuf },
}

impl FirmwareSource {
    /// Construct a [`FirmwareSource`] from a CLI-provided firmware path.
    ///
    /// `allow_unverified` is the CLI opt-in flag; without it a local firmware
    /// file that does not belong to an upstream firmware directory is
    /// rejected.
    pub fn from_cli(
        firmware_bundle_path: Option<PathBuf>,
        allow_unverified: bool,
    ) -> Result<Option<Self>> {
        let Some(path) = firmware_bundle_path else {
            return Ok(None);
        };
        let path = resolve_firmware_path(&path)?;
        if path.is_dir() {
            let code_path = path.join(UPSTREAM_CODE_FILE);
            let vars_path = path.join(UPSTREAM_VARS_FILE);
            if !code_path.is_file() {
                bail!(
                    "firmware directory {} is missing {}; prepare the Ostool firmware cache first",
                    path.display(),
                    UPSTREAM_CODE_FILE
                );
            }
            if !vars_path.is_file() {
                bail!(
                    "firmware directory {} is missing {}",
                    path.display(),
                    UPSTREAM_VARS_FILE
                );
            }
            return Ok(Some(Self::Upstream {
                code_path,
                vars_path,
            }));
        }
        if !allow_unverified {
            bail!(
                "firmware path {} is not an upstream firmware directory; pass \
                 --allow-unverified-firmware to use a local firmware file",
                path.display()
            );
        }
        Ok(Some(Self::UnverifiedLocal { code_path: path }))
    }
}

/// Fully validated result of verifying a firmware source.
///
/// The fields are the pieces later tasks need (loader GPA layout, file
/// paths, digests, verification label); the struct is immutable once built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOvmfBundle {
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
    /// Path of the VARS image to load.
    pub vars_path: PathBuf,
    /// SHA-256 of the CODE image.
    pub code_sha256: String,
    /// SHA-256 of the VARS image.
    pub vars_sha256: String,
    /// `true` when the firmware passed full layout verification.
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
/// Upstream firmware directories are verified against the fixed layout
/// constants; unverified local firmware is checked for the fixed CODE size
/// only and its SHA-256 is printed.
pub fn verify_firmware(source: &FirmwareSource) -> Result<VerifiedOvmfBundle> {
    match source {
        FirmwareSource::Upstream {
            code_path,
            vars_path,
        } => verify_upstream_firmware(code_path, vars_path),
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

fn verify_upstream_firmware(code_path: &Path, vars_path: &Path) -> Result<VerifiedOvmfBundle> {
    verify_layout_file(code_path, "code", OVMF_CODE_SIZE)?;
    verify_layout_file(vars_path, "vars", OVMF_VARS_SIZE)?;

    let code_sha256 = file_sha256(code_path)?;
    let vars_sha256 = file_sha256(vars_path)?;

    let code_end = OVMF_CODE_BASE
        .checked_add(OVMF_CODE_SIZE)
        .ok_or_else(|| anyhow!("code_base + code_size overflows u64"))?;
    let reset_vector_end = OVMF_RESET_VECTOR
        .checked_add(16)
        .ok_or_else(|| anyhow!("reset_vector + 16 overflows u64"))?;
    if OVMF_RESET_VECTOR < OVMF_CODE_BASE || reset_vector_end > code_end {
        bail!(
            "reset vector 0x{OVMF_RESET_VECTOR:x} is outside the CODE window [0x{:x}, 0x{:x})",
            OVMF_CODE_BASE,
            code_end
        );
    }

    let bundle = VerifiedOvmfBundle {
        code_base: OVMF_CODE_BASE,
        code_size: OVMF_CODE_SIZE,
        vars_base: OVMF_VARS_BASE,
        vars_size: OVMF_VARS_SIZE,
        combined_size: OVMF_COMBINED_SIZE,
        code_path: code_path.to_path_buf(),
        vars_path: vars_path.to_path_buf(),
        code_sha256,
        vars_sha256,
        verified: true,
    };
    print_selected_firmware(&bundle);
    Ok(bundle)
}

fn verify_layout_file(path: &Path, role: &str, expected_size: u64) -> Result<()> {
    let actual_size = fs::metadata(path)
        .with_context(|| format!("failed to read metadata of {}", path.display()))?
        .len();
    if actual_size != expected_size {
        bail!(
            "{} firmware {} has size {}; expected {}",
            role,
            path.display(),
            actual_size,
            expected_size
        );
    }
    Ok(())
}

fn verify_unverified_local_firmware(code_path: &Path) -> Result<VerifiedOvmfBundle> {
    if !code_path.is_file() {
        bail!(
            "unverified UEFI firmware does not exist: {}",
            code_path.display()
        );
    }
    verify_layout_file(code_path, "unverified code", OVMF_CODE_SIZE)?;
    let code_sha256 = file_sha256(code_path)?;
    println!(
        "unverified firmware: path={}, size={}, sha256={}",
        code_path.display(),
        OVMF_CODE_SIZE,
        code_sha256
    );
    let bundle = VerifiedOvmfBundle {
        code_base: OVMF_CODE_BASE,
        code_size: OVMF_CODE_SIZE,
        vars_base: OVMF_VARS_BASE,
        vars_size: OVMF_VARS_SIZE,
        combined_size: OVMF_COMBINED_SIZE,
        code_path: code_path.to_path_buf(),
        vars_path: code_path.with_file_name(UPSTREAM_VARS_FILE),
        code_sha256,
        vars_sha256: String::new(),
        verified: false,
    };
    print_selected_firmware(&bundle);
    eprintln!(
        "WARNING: UNVERIFIED firmware is diagnostic-only and must not determine UEFI test results."
    );
    Ok(bundle)
}

fn print_selected_firmware(bundle: &VerifiedOvmfBundle) {
    println!("selected firmware profile: {OVMF_PROFILE_NAME}");
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
    if !bundle.vars_sha256.is_empty() {
        println!("  vars_sha256={}", bundle.vars_sha256);
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

#[cfg(test)]
mod tests {
    use std::fs;

    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn verifies_an_upstream_firmware_directory() {
        let dir = tempdir().unwrap();
        let code_path = write_file_with_size(dir.path().join(UPSTREAM_CODE_FILE), OVMF_CODE_SIZE);
        let vars_path = write_file_with_size(dir.path().join(UPSTREAM_VARS_FILE), OVMF_VARS_SIZE);

        let bundle = verify_firmware(&FirmwareSource::Upstream {
            code_path: code_path.clone(),
            vars_path: vars_path.clone(),
        })
        .unwrap();

        assert_eq!(bundle.code_base, OVMF_CODE_BASE);
        assert_eq!(bundle.code_size, OVMF_CODE_SIZE);
        assert_eq!(bundle.vars_base, OVMF_VARS_BASE);
        assert_eq!(bundle.vars_size, OVMF_VARS_SIZE);
        assert_eq!(bundle.combined_size, OVMF_COMBINED_SIZE);
        assert_eq!(bundle.code_path, code_path);
        assert_eq!(bundle.vars_path, vars_path);
        assert!(bundle.verified);
        assert!(bundle.reset_vector_in_code_window());
        assert_eq!(bundle.code_end(), OVMF_CODE_BASE + OVMF_CODE_SIZE);
    }

    #[test]
    fn rejects_upstream_firmware_with_wrong_code_size() {
        let dir = tempdir().unwrap();
        let code_path = write_file_with_size(dir.path().join(UPSTREAM_CODE_FILE), 0x1000);
        let vars_path = write_file_with_size(dir.path().join(UPSTREAM_VARS_FILE), OVMF_VARS_SIZE);

        let err = format!(
            "{:#}",
            verify_firmware(&FirmwareSource::Upstream {
                code_path,
                vars_path
            })
            .unwrap_err()
        );

        assert!(
            err.contains("code") && err.contains(&OVMF_CODE_SIZE.to_string()),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_upstream_firmware_with_wrong_vars_size() {
        let dir = tempdir().unwrap();
        let code_path = write_file_with_size(dir.path().join(UPSTREAM_CODE_FILE), OVMF_CODE_SIZE);
        let vars_path = write_file_with_size(dir.path().join(UPSTREAM_VARS_FILE), 0x1000);

        let err = format!(
            "{:#}",
            verify_firmware(&FirmwareSource::Upstream {
                code_path,
                vars_path
            })
            .unwrap_err()
        );

        assert!(
            err.contains("vars") && err.contains(&OVMF_VARS_SIZE.to_string()),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn upstream_directory_requires_code_and_vars_present() {
        let dir = tempdir().unwrap();
        write_file_with_size(dir.path().join(UPSTREAM_CODE_FILE), OVMF_CODE_SIZE);

        let err = format!(
            "{:?}",
            FirmwareSource::from_cli(Some(dir.path().to_path_buf()), false)
                .unwrap_err()
                .to_string()
        );

        assert!(
            err.contains("missing") && err.contains(UPSTREAM_VARS_FILE),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_unverified_firmware_without_opt_in() {
        let dir = tempdir().unwrap();
        let code_path = dir.path().join("local-code.fd");
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
        let code_path = dir.path().join("local-code.fd");
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
        let code_path = dir.path().join("local-code.fd");
        fs::write(&code_path, vec![0xa5; 0x1000]).unwrap();

        let source = FirmwareSource::from_cli(Some(code_path.clone()), true).unwrap();
        let source = source.unwrap();
        let err = format!("{:?}", verify_firmware(&source).unwrap_err().to_string());

        assert!(
            err.contains(&OVMF_CODE_SIZE.to_string()),
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

    fn write_file_with_size(path: PathBuf, size: u64) -> PathBuf {
        fs::write(&path, vec![0xa5; size as usize]).unwrap();
        path
    }

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
}
