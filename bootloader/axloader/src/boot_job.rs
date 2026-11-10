//! Platform-neutral HTTP boot job storage and validation.

extern crate alloc;

use alloc::{format, string::String, vec::Vec};

use httpboot_protocol::{DeviceBootImage, DeviceBootJob, DeviceBootStatus, DeviceOperationError};
use sha2::{Digest, Sha256};

use crate::integrity;

/// The file carried by a boot job upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Kernel,
    Initramfs,
}

/// A validated manifest and the immutable files received for it.
pub struct BootJob {
    manifest: DeviceBootJob,
    kernel: Option<Vec<u8>>,
    initramfs: Option<Vec<u8>>,
    failure: Option<DeviceOperationError>,
}

impl BootJob {
    /// Returns the immutable manifest.
    pub fn manifest(&self) -> &DeviceBootJob {
        &self.manifest
    }

    /// Borrows the immutable kernel upload.
    pub fn kernel(&self) -> Option<&[u8]> {
        self.kernel.as_deref()
    }

    /// Borrows the immutable initramfs upload.
    pub fn initramfs(&self) -> Option<&[u8]> {
        self.initramfs.as_deref()
    }
}

/// Stores at most one in-flight boot transaction.
#[derive(Default)]
pub struct BootJobStore {
    job: Option<BootJob>,
}

impl BootJobStore {
    /// Returns whether a transaction currently owns the boot slot.
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Encodes the protocol status for the current transaction.
    pub fn status(&self) -> Option<DeviceBootStatus> {
        self.job.as_ref().map(status_of)
    }

    /// Encodes the status for exactly `id`, including a failed transaction.
    pub fn status_for(&self, id: &str) -> Result<DeviceBootStatus, DeviceOperationError> {
        self.stored_job(id).map(status_of)
    }

    /// Creates a manifest, or accepts an identical retry of the current one.
    pub fn create(
        &mut self,
        manifest: DeviceBootJob,
        max_kernel_bytes: usize,
        max_initramfs_bytes: usize,
    ) -> Result<bool, DeviceOperationError> {
        validate_manifest(&manifest, max_kernel_bytes, max_initramfs_bytes)?;
        if let Some(job) = &self.job {
            return if is_idempotent_boot_job(job.manifest(), &manifest) {
                Ok(false)
            } else {
                Err(operation_error(
                    "boot_job_busy",
                    "another boot job is already active",
                ))
            };
        }
        self.job = Some(BootJob {
            manifest,
            kernel: None,
            initramfs: None,
            failure: None,
        });
        Ok(true)
    }

    /// Returns the descriptor for one requested file.
    pub fn descriptor(
        &self,
        id: &str,
        kind: FileKind,
    ) -> Result<&DeviceBootImage, DeviceOperationError> {
        let job = self.job_for(id)?;
        match kind {
            FileKind::Kernel => Ok(&job.manifest.kernel),
            FileKind::Initramfs => job.manifest.initramfs.as_ref().ok_or_else(|| {
                operation_error(
                    "initramfs_not_requested",
                    "boot manifest does not request an initramfs",
                )
            }),
        }
    }

    /// Returns the first terminal error for `id`, if one is recorded.
    ///
    /// Frontends use this before reading a PUT/start body so a failed job
    /// replays its original error without retaining another upload buffer.
    pub fn failure(&self, id: &str) -> Result<Option<&DeviceOperationError>, DeviceOperationError> {
        self.stored_job(id).map(|job| job.failure.as_ref())
    }

    /// Stores a complete file after checking its declared size and digest.
    pub fn upload(
        &mut self,
        id: &str,
        kind: FileKind,
        data: Vec<u8>,
    ) -> Result<(), DeviceOperationError> {
        let (expected_size, expected_digest) = {
            let descriptor = self.descriptor(id, kind)?;
            (
                descriptor.size,
                integrity::decode_sha256(&descriptor.sha256),
            )
        };
        if expected_size != data.len() as u64 {
            let error = operation_error(
                "image_size_mismatch",
                format!(
                    "{} upload length {} does not match declared length {}",
                    file_name(kind),
                    data.len(),
                    expected_size
                ),
            );
            self.fail(id, error.clone())?;
            return Err(error);
        }
        let actual_digest = Sha256::digest(&data).into();
        if expected_digest.as_ref() != Some(&actual_digest) {
            let error = operation_error(
                "image_digest_mismatch",
                format!(
                    "{} upload SHA-256 does not match the manifest",
                    file_name(kind)
                ),
            );
            self.fail(id, error.clone())?;
            return Err(error);
        }
        let job = self
            .job
            .as_mut()
            .expect("a successful descriptor lookup requires a job");
        match kind {
            FileKind::Kernel => job.kernel = Some(data),
            FileKind::Initramfs => job.initramfs = Some(data),
        }
        Ok(())
    }

    /// Cancels a transaction by ID.
    pub fn cancel(&mut self, id: &str) -> Result<(), DeviceOperationError> {
        self.stored_job(id)?;
        self.job = None;
        Ok(())
    }

    /// Returns the immutable job borrowed by a backend loader.
    pub fn job(&self, id: &str) -> Result<&BootJob, DeviceOperationError> {
        self.job_for(id)
    }

    /// Records the first terminal failure and releases source upload buffers.
    pub fn fail(
        &mut self,
        id: &str,
        error: DeviceOperationError,
    ) -> Result<(), DeviceOperationError> {
        let job = self
            .job
            .as_mut()
            .filter(|job| job.manifest.boot_id == id)
            .ok_or_else(unknown_boot_job)?;
        if job.failure.is_some() {
            return Ok(());
        }
        job.failure = Some(error);
        job.kernel.take();
        job.initramfs.take();
        Ok(())
    }

    fn job_for(&self, id: &str) -> Result<&BootJob, DeviceOperationError> {
        let job = self.stored_job(id)?;
        if let Some(error) = &job.failure {
            return Err(error.clone());
        }
        Ok(job)
    }

    fn stored_job(&self, id: &str) -> Result<&BootJob, DeviceOperationError> {
        self.job
            .as_ref()
            .filter(|job| job.manifest.boot_id == id)
            .ok_or_else(unknown_boot_job)
    }
}

fn status_of(job: &BootJob) -> DeviceBootStatus {
    let failed = job.failure.is_some();
    let kernel_received = !failed && job.kernel.is_some();
    let initramfs_received = !failed && job.initramfs.is_some();
    let ready = kernel_received && (job.manifest.initramfs.is_none() || initramfs_received);
    DeviceBootStatus {
        boot_id: job.manifest.boot_id.clone(),
        phase: if failed {
            "failed"
        } else if ready {
            "ready"
        } else {
            "receiving"
        }
        .into(),
        kernel_received,
        initramfs_received,
        last_error: job.failure.clone(),
    }
}

fn validate_manifest(
    manifest: &DeviceBootJob,
    max_kernel_bytes: usize,
    max_initramfs_bytes: usize,
) -> Result<(), DeviceOperationError> {
    if manifest.boot_id.is_empty()
        || manifest.boot_id.len() > 96
        || !manifest
            .boot_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(operation_error(
            "invalid_boot_manifest",
            "boot_id must contain 1-96 ASCII alphanumeric, '-' or '_' characters",
        ));
    }
    validate_image(&manifest.kernel, max_kernel_bytes, "kernel")?;
    if let Some(initramfs) = &manifest.initramfs {
        validate_image(initramfs, max_initramfs_bytes, "initramfs")?;
    }
    if manifest
        .cmdline
        .as_deref()
        .is_some_and(|cmdline| !httpboot_protocol::valid_host_cmdline(cmdline))
    {
        return Err(operation_error(
            "invalid_boot_manifest",
            "cmdline is outside the protocol limits",
        ));
    }
    Ok(())
}

fn validate_image(
    image: &DeviceBootImage,
    max_size: usize,
    name: &str,
) -> Result<(), DeviceOperationError> {
    if image.size == 0
        || image.size > max_size as u64
        || integrity::decode_sha256(&image.sha256).is_none()
    {
        return Err(operation_error(
            if image.size > max_size as u64 {
                "image_too_large"
            } else {
                "invalid_boot_manifest"
            },
            format!(
                "{name} size {} or SHA-256 is invalid (maximum {max_size})",
                image.size
            ),
        ));
    }
    Ok(())
}

fn is_idempotent_boot_job(existing: &DeviceBootJob, incoming: &DeviceBootJob) -> bool {
    existing == incoming
}

fn file_name(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Kernel => "kernel",
        FileKind::Initramfs => "initramfs",
    }
}

fn unknown_boot_job() -> DeviceOperationError {
    operation_error("unknown_boot_job", "boot job does not exist")
}

/// Builds a device control error with a stable machine-readable code.
pub fn operation_error(
    code: impl Into<String>,
    message: impl Into<String>,
) -> DeviceOperationError {
    DeviceOperationError {
        code: code.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use alloc::{format, string::String, vec};

    use httpboot_protocol::{DeviceBootImage, DeviceBootJob};
    use sha2::{Digest, Sha256};

    use super::{BootJobStore, FileKind};

    fn image(data: &[u8]) -> DeviceBootImage {
        DeviceBootImage {
            size: data.len() as u64,
            sha256: hex(&Sha256::digest(data)),
        }
    }

    fn manifest(id: &str, kernel: &[u8]) -> DeviceBootJob {
        DeviceBootJob {
            boot_id: id.into(),
            kernel: image(kernel),
            initramfs: None,
            cmdline: None,
        }
    }

    fn hex(bytes: &[u8]) -> String {
        use core::fmt::Write;

        let mut output = String::new();
        for byte in bytes {
            write!(output, "{byte:02x}").unwrap();
        }
        output
    }

    #[test]
    fn duplicate_manifest_is_idempotent_and_conflict_preserves_the_job() {
        let mut jobs = BootJobStore::default();
        let first = manifest("same", &[7]);
        assert!(matches!(jobs.create(first.clone(), 1, 1), Ok(true)));
        assert!(matches!(jobs.create(first.clone(), 1, 1), Ok(false)));
        assert_eq!(
            jobs.create(manifest("other", &[7]), 1, 1).unwrap_err().code,
            "boot_job_busy"
        );
        assert_eq!(jobs.status_for("same").unwrap().phase, "receiving");
    }

    #[test]
    fn upload_failure_is_terminal_replays_first_error_and_releases_buffers() {
        let mut jobs = BootJobStore::default();
        let manifest = manifest("failed", &[7]);
        assert!(jobs.create(manifest.clone(), 1, 1).is_ok());
        assert_eq!(
            jobs.upload("failed", FileKind::Kernel, vec![2])
                .unwrap_err()
                .code,
            "image_digest_mismatch"
        );
        let status = jobs.status_for("failed").unwrap();
        assert_eq!(status.phase, "failed");
        assert!(!status.kernel_received);
        assert_eq!(
            status.last_error.as_ref().map(|error| error.code.as_str()),
            Some("image_digest_mismatch")
        );
        assert_eq!(
            jobs.failure("failed").unwrap().unwrap().code,
            "image_digest_mismatch"
        );

        assert_eq!(
            jobs.upload("failed", FileKind::Kernel, vec![7])
                .unwrap_err()
                .code,
            "image_digest_mismatch"
        );
        jobs.fail(
            "failed",
            super::operation_error("later_error", "must not replace the first error"),
        )
        .unwrap();
        assert_eq!(
            jobs.status_for("failed").unwrap().last_error.unwrap().code,
            "image_digest_mismatch"
        );
        assert!(matches!(jobs.create(manifest, 1, 1), Ok(false)));
        assert_eq!(jobs.status_for("failed").unwrap().phase, "failed");
        assert!(jobs.cancel("failed").is_ok());
        assert_eq!(
            jobs.status_for("failed").unwrap_err().code,
            "unknown_boot_job"
        );
    }

    #[test]
    fn optional_payload_combinations_reach_ready() {
        for (initramfs, cmdline) in [
            (None, None),
            (None, Some("console=ttyS0")),
            (Some(vec![1, 2]), None),
            (Some(vec![1, 2]), Some("console=ttyS0")),
        ] {
            let mut jobs = BootJobStore::default();
            let mut job = manifest("payload", &[7]);
            job.initramfs = initramfs.as_deref().map(image);
            job.cmdline = cmdline.map(String::from);
            assert!(jobs.create(job, 1, 2).is_ok());
            jobs.upload("payload", FileKind::Kernel, vec![7]).unwrap();
            if let Some(initramfs) = initramfs {
                jobs.upload("payload", FileKind::Initramfs, initramfs)
                    .unwrap();
            }
            assert_eq!(jobs.status_for("payload").unwrap().phase, "ready");
        }
    }

    #[test]
    fn upload_size_mismatch_fails_before_digest_validation() {
        let mut jobs = BootJobStore::default();
        assert!(jobs.create(manifest("short", &[7]), 1, 1).is_ok());
        let error = jobs
            .upload("short", FileKind::Kernel, Vec::new())
            .unwrap_err();
        assert_eq!(error.code, "image_size_mismatch");
        assert_eq!(jobs.status_for("short").unwrap().phase, "failed");
    }

    #[test]
    fn v7_manifest_rejects_removed_arch_format_and_entry_fields() {
        for legacy_field in [
            r#""arch":"x86_64""#,
            r#""image_format":"elf64""#,
            r#""entry_symbol":"__x86_64_efi_pe_entry""#,
        ] {
            let json = format!(
                r#"{{"boot_id":"legacy",{legacy_field},"kernel":{{"size":1,"sha256":"{}"}}}}"#,
                "00".repeat(32)
            );
            assert!(serde_json::from_str::<DeviceBootJob>(&json).is_err());
        }
    }

    #[test]
    fn invalid_manifest_is_rejected_before_busy_conflict() {
        let mut jobs = BootJobStore::default();
        assert!(matches!(
            jobs.create(manifest("active", &[7]), 1, 1),
            Ok(true)
        ));

        let mut invalid = manifest("", &[7]);
        invalid.cmdline = Some("valid".into());
        assert_eq!(
            jobs.create(invalid, 1, 1).unwrap_err().code,
            "invalid_boot_manifest"
        );

        let oversized = manifest("too-large", &[7, 7]);
        assert_eq!(
            jobs.create(oversized, 1, 1).unwrap_err().code,
            "image_too_large"
        );
        assert_eq!(jobs.status_for("active").unwrap().phase, "receiving");
    }

    #[test]
    fn unknown_id_does_not_change_the_current_transaction() {
        let mut jobs = BootJobStore::default();
        assert!(matches!(
            jobs.create(manifest("active", &[7]), 1, 1),
            Ok(true)
        ));
        assert_eq!(
            jobs.upload("missing", FileKind::Kernel, alloc::vec![0])
                .unwrap_err()
                .code,
            "unknown_boot_job"
        );
        assert_eq!(
            jobs.failure("missing").unwrap_err().code,
            "unknown_boot_job"
        );
        assert_eq!(jobs.status_for("active").unwrap().phase, "receiving");
        assert!(matches!(
            jobs.create(manifest("active", &[7]), 1, 1),
            Ok(false)
        ));
    }
}
