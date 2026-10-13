//! Device-owned, volatile HTTP boot transaction.

extern crate alloc;

use alloc::{rc::Rc, string::String};
use core::{cell::RefCell, ptr::NonNull};

pub use axloader::boot_job::FileKind;
use axloader::{boot_job::BootJobStore, target::X86_64};
use httpboot_protocol::{
    DeviceBootJob, LoaderHardwareInfo, MAX_HTTP_BOOT_INITRAMFS_BYTES, MacAddress,
};
use uefi::boot;

use super::{elf_loader, entry, payload};

pub struct BootServer {
    epoch: String,
    mac_address: MacAddress,
    current_mac_address: MacAddress,
    hardware: LoaderHardwareInfo,
    jobs: BootJobStore,
    pub serial: Rc<RefCell<httpboot_protocol::LoaderSerialStatus>>,
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
            jobs: BootJobStore::default(),
            serial,
        }
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn busy(&self) -> bool {
        self.jobs.busy()
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

    pub fn status(&self) -> Option<httpboot_protocol::DeviceBootStatus> {
        self.jobs.status()
    }

    pub fn create(&mut self, manifest: DeviceBootJob) -> Result<bool, &'static str> {
        self.jobs.create(
            manifest,
            X86_64,
            MAX_HTTP_BOOT_INITRAMFS_BYTES,
            MAX_HTTP_BOOT_INITRAMFS_BYTES,
        )
    }

    pub fn descriptor(
        &self,
        id: &str,
        kind: FileKind,
    ) -> Result<&httpboot_protocol::DeviceBootImage, &'static str> {
        self.jobs.descriptor(id, kind)
    }

    pub fn upload(
        &mut self,
        id: &str,
        kind: FileKind,
        data: alloc::vec::Vec<u8>,
    ) -> Result<(), &'static str> {
        self.jobs.upload(id, kind, data)
    }

    pub fn cancel(&mut self, id: &str) -> Result<(), &'static str> {
        self.jobs.cancel(id)
    }

    pub fn prepare(
        &mut self,
        id: &str,
        binding: Option<&str>,
    ) -> Result<BootExecution, &'static str> {
        if !binding.is_some_and(|id| self.serial.borrow().permits_start(id)) {
            return Err("serial_binding_required");
        }
        let job = self.jobs.job().ok_or("unknown_boot_job")?;
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
                self.jobs.clear();
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
                self.jobs.clear();
                return Err("host_payload_failed");
            }
        };
        let payload = match payload {
            Ok(payload) => payload,
            Err(_) => {
                free_loaded_elf(&elf);
                self.jobs.clear();
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
