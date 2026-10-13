//! Platform-neutral HTTP boot job storage and validation.

extern crate alloc;

use alloc::vec::Vec;

use httpboot_protocol::{DeviceBootImage, DeviceBootJob, DeviceBootStatus};
use sha2::{Digest, Sha256};

use crate::{
    integrity,
    target::{TargetSpec, is_idempotent_boot_job},
};

/// The file carried by a boot job upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Kernel,
    Initramfs,
}

/// A validated manifest and the files received for it.
pub struct BootJob {
    pub manifest: DeviceBootJob,
    pub kernel: Option<Vec<u8>>,
    pub initramfs: Option<Vec<u8>>,
}

/// Stores at most one in-flight boot transaction.
#[derive(Default)]
pub struct BootJobStore {
    job: Option<BootJob>,
}

impl BootJobStore {
    /// Returns whether a transaction is currently receiving files.
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Encodes the protocol status for the current transaction.
    pub fn status(&self) -> Option<DeviceBootStatus> {
        self.job.as_ref().map(|job| DeviceBootStatus {
            boot_id: job.manifest.boot_id.clone(),
            phase: if job.kernel.is_some()
                && (job.manifest.initramfs.is_none() || job.initramfs.is_some())
            {
                "ready"
            } else {
                "receiving"
            }
            .into(),
            kernel_received: job.kernel.is_some(),
            initramfs_received: job.initramfs.is_some(),
            last_error: None,
        })
    }

    /// Creates a manifest, or accepts an identical retry of the current one.
    pub fn create(
        &mut self,
        manifest: DeviceBootJob,
        target: TargetSpec,
        max_kernel_bytes: usize,
        max_initramfs_bytes: usize,
    ) -> Result<bool, &'static str> {
        if let Some(job) = &self.job {
            return if is_idempotent_boot_job(&job.manifest, &manifest) {
                Ok(false)
            } else {
                Err("boot_job_busy")
            };
        }
        if manifest.boot_id.is_empty()
            || manifest.boot_id.len() > 96
            || !manifest
                .boot_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            || !target.accepts(
                manifest.arch,
                manifest.image_format,
                manifest.entry_symbol.as_deref(),
            )
            || !valid_image(&manifest.kernel, max_kernel_bytes)
            || manifest
                .initramfs
                .as_ref()
                .is_some_and(|image| !valid_image(image, max_initramfs_bytes))
            || manifest
                .cmdline
                .as_deref()
                .is_some_and(|cmdline| !httpboot_protocol::valid_host_cmdline(cmdline))
        {
            return Err("invalid_boot_manifest");
        }
        self.job = Some(BootJob {
            manifest,
            kernel: None,
            initramfs: None,
        });
        Ok(true)
    }

    /// Returns the descriptor for one requested file.
    pub fn descriptor(&self, id: &str, kind: FileKind) -> Result<&DeviceBootImage, &'static str> {
        let job = self.job.as_ref().ok_or("unknown_boot_job")?;
        if job.manifest.boot_id != id {
            return Err("unknown_boot_job");
        }
        match kind {
            FileKind::Kernel => Ok(&job.manifest.kernel),
            FileKind::Initramfs => job
                .manifest
                .initramfs
                .as_ref()
                .ok_or("initramfs_not_requested"),
        }
    }

    /// Stores a complete file after checking its declared size and digest.
    pub fn upload(&mut self, id: &str, kind: FileKind, data: Vec<u8>) -> Result<(), &'static str> {
        let descriptor = self.descriptor(id, kind)?;
        if descriptor.size != data.len() as u64
            || integrity::decode_sha256(&descriptor.sha256).as_ref()
                != Some(&Sha256::digest(&data).into())
        {
            self.job = None;
            return Err("image_digest_mismatch");
        }
        let job = self.job.as_mut().expect("descriptor requires a job");
        match kind {
            FileKind::Kernel => job.kernel = Some(data),
            FileKind::Initramfs => job.initramfs = Some(data),
        }
        Ok(())
    }

    /// Cancels a transaction by ID.
    pub fn cancel(&mut self, id: &str) -> Result<(), &'static str> {
        self.descriptor(id, FileKind::Kernel)?;
        self.job = None;
        Ok(())
    }

    /// Returns the current job to a platform-specific loader.
    pub fn job(&self) -> Option<&BootJob> {
        self.job.as_ref()
    }

    /// Drops a transaction after a platform-specific validation failure.
    pub fn clear(&mut self) {
        self.job = None;
    }
}

fn valid_image(image: &DeviceBootImage, max_size: usize) -> bool {
    image.size > 0
        && image.size <= max_size as u64
        && integrity::decode_sha256(&image.sha256).is_some()
}

#[cfg(test)]
mod tests {
    use httpboot_protocol::{BootArch, DeviceBootJob, ImageFormat};

    use super::{BootJobStore, FileKind};
    use crate::target::{LOONGARCH64_EFI, X86_64};

    fn manifest(id: &str) -> DeviceBootJob {
        serde_json::from_value(serde_json::json!({
            "boot_id": id,
            "arch": "x86_64",
            "image_format": "elf64",
            "kernel": { "size": 1, "sha256": "00".repeat(32) },
            "entry_symbol": "__x86_64_efi_pe_entry"
        }))
        .unwrap()
    }

    fn loong_manifest(id: &str) -> DeviceBootJob {
        serde_json::from_value(serde_json::json!({
            "boot_id": id,
            "arch": "loongarch64",
            "image_format": "elf64",
            "kernel": {
                "size": 1,
                "sha256": "ca358758f6d27e6cf45272937977a748fd88391db679ceda7dc7bf1f005ee879"
            },
            "entry_symbol": "efi_pe_entry"
        }))
        .unwrap()
    }

    #[test]
    fn duplicate_manifest_is_idempotent_and_conflicting_manifest_is_busy() {
        let mut jobs = BootJobStore::default();
        let first = manifest("same");
        assert!(jobs.create(first.clone(), X86_64, 1, 1).is_ok());
        assert_eq!(jobs.create(first, X86_64, 1, 1), Ok(false));
        let mut different = manifest("other");
        different.arch = BootArch::Loongarch64;
        different.image_format = ImageFormat::Elf64;
        assert_eq!(jobs.create(different, X86_64, 1, 1), Err("boot_job_busy"));
    }

    #[test]
    fn loong_manifest_retry_and_upload_reach_ready() {
        let mut jobs = BootJobStore::default();
        let first = loong_manifest("loong");
        assert_eq!(jobs.create(first.clone(), LOONGARCH64_EFI, 1, 1), Ok(true));
        assert_eq!(jobs.create(first, LOONGARCH64_EFI, 1, 1), Ok(false));
        assert_eq!(jobs.upload("loong", FileKind::Kernel, vec![7]), Ok(()));
        assert_eq!(jobs.status().unwrap().phase, "ready");

        let mut different = loong_manifest("other");
        different.entry_symbol = Some("wrong_entry".into());
        assert_eq!(
            jobs.create(different, LOONGARCH64_EFI, 1, 1),
            Err("boot_job_busy")
        );
    }
}
