//! Device-owned, volatile HTTP boot transaction.

extern crate alloc;

use alloc::{rc::Rc, string::String, vec::Vec};
use core::{cell::RefCell, ptr::NonNull};

use httpboot_protocol::{
    BootArch, DeviceBootImage, DeviceBootJob, DeviceBootStatus, ImageFormat, LoaderHardwareInfo,
    MAX_HTTP_BOOT_INITRAMFS_BYTES, MacAddress,
};
use sha2::{Digest, Sha256};
use uefi::boot;

use super::{elf_loader, entry, payload};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Kernel,
    Initramfs,
}

pub struct BootServer {
    epoch: String,
    mac_address: MacAddress,
    current_mac_address: MacAddress,
    hardware: LoaderHardwareInfo,
    job: Option<Job>,
    pub serial: Rc<RefCell<httpboot_protocol::LoaderSerialStatus>>,
}

struct Job {
    manifest: DeviceBootJob,
    kernel: Option<Vec<u8>>,
    initramfs: Option<Vec<u8>>,
    last_error: Option<String>,
}

pub struct BootExecution {
    pub elf: elf_loader::LoadedElf,
    pub payload: payload::PublishedPayload,
    pub load_options: entry::PreparedLoadOptions,
}

impl BootServer {
    pub fn new(
        epoch: String,
        mac_address: MacAddress,
        current_mac_address: MacAddress,
        hardware: LoaderHardwareInfo,
        serial: Rc<RefCell<httpboot_protocol::LoaderSerialStatus>>,
    ) -> Self {
        Self {
            epoch,
            mac_address,
            current_mac_address,
            hardware,
            job: None,
            serial,
        }
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn mac_address(&self) -> MacAddress {
        self.mac_address
    }

    pub fn current_mac_address(&self) -> MacAddress {
        self.current_mac_address
    }

    pub fn hardware(&self) -> &LoaderHardwareInfo {
        &self.hardware
    }

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
            last_error: job.last_error.clone(),
        })
    }

    pub fn create(&mut self, manifest: DeviceBootJob) -> Result<bool, &'static str> {
        if let Some(job) = &self.job {
            return if job.manifest == manifest {
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
            || manifest.arch != BootArch::X86_64
            || manifest.image_format != ImageFormat::Elf64
            || !valid_image(&manifest.kernel)
            || manifest
                .initramfs
                .as_ref()
                .is_some_and(|image| !valid_image(image))
            || manifest
                .cmdline
                .as_deref()
                .is_some_and(|cmdline| !httpboot_protocol::valid_host_cmdline(cmdline))
            || manifest.entry_symbol.as_deref() != Some(elf_loader::EFI_ENTRY_SYMBOL)
        {
            return Err("invalid_boot_manifest");
        }
        self.job = Some(Job {
            manifest,
            kernel: None,
            initramfs: None,
            last_error: None,
        });
        Ok(true)
    }

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

    pub fn upload(&mut self, id: &str, kind: FileKind, data: Vec<u8>) -> Result<(), &'static str> {
        let desc = self.descriptor(id, kind)?;
        if desc.size != data.len() as u64
            || axloader::integrity::decode_sha256(&desc.sha256).as_ref()
                != Some(&Sha256::digest(&data).into())
        {
            self.job = None;
            return Err("image_digest_mismatch");
        }
        let job = self.job.as_mut().expect("descriptor requires a job");
        let target = match kind {
            FileKind::Kernel => &mut job.kernel,
            FileKind::Initramfs => &mut job.initramfs,
        };
        *target = Some(data);
        job.last_error = None;
        Ok(())
    }

    pub fn cancel(&mut self, id: &str) -> Result<(), &'static str> {
        self.descriptor(id, FileKind::Kernel)?;
        self.job = None;
        Ok(())
    }

    pub fn prepare(
        &mut self,
        id: &str,
        binding: Option<&str>,
    ) -> Result<BootExecution, &'static str> {
        if !binding.is_some_and(|id| self.serial.borrow().permits_start(id)) {
            return Err("serial_binding_required");
        }
        let job = self.job.as_ref().ok_or("unknown_boot_job")?;
        if job.manifest.boot_id != id {
            return Err("unknown_boot_job");
        }
        let bytes = job.kernel.as_ref().ok_or("kernel_not_uploaded")?;
        if job.manifest.initramfs.is_some() && job.initramfs.is_none() {
            return Err("initramfs_not_uploaded");
        }
        let elf = match elf_loader::load_elf(bytes, job.manifest.entry_symbol.as_deref()) {
            Ok(elf) => elf,
            Err(_) => {
                self.job = None;
                return Err("kernel_load_failed");
            }
        };
        let load_options = entry::PreparedLoadOptions::new(job.manifest.cmdline.as_deref());
        let payload = payload::prepare_uploaded(job.initramfs.as_deref())
            .and_then(payload::PreparedPayload::publish);
        let load_options = match load_options {
            Ok(load_options) => load_options,
            Err(_) => {
                free_loaded_elf(&elf);
                self.job = None;
                return Err("host_payload_failed");
            }
        };
        let payload = match payload {
            Ok(payload) => payload,
            Err(_) => {
                free_loaded_elf(&elf);
                self.job = None;
                return Err("host_payload_failed");
            }
        };
        Ok(BootExecution {
            elf,
            payload,
            load_options,
        })
    }
}

pub(super) fn free_loaded_elf(elf: &elf_loader::LoadedElf) {
    // SAFETY: this ELF region has not been handed to the kernel.
    unsafe {
        boot::free_pages(
            NonNull::new(elf.load_addr as *mut u8).expect("allocated ELF address"),
            elf.page_count,
        )
    }
    .expect("failed to free ELF pages");
}

fn valid_image(image: &DeviceBootImage) -> bool {
    image.size > 0
        && image.size <= MAX_HTTP_BOOT_INITRAMFS_BYTES as u64
        && axloader::integrity::decode_sha256(&image.sha256).is_some()
}
