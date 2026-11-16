//! Device-owned, volatile HTTP boot transaction.

extern crate alloc;

use alloc::{format, rc::Rc, string::String};
use core::cell::RefCell;

pub use axloader::boot_job::FileKind;
use axloader::boot_job::{BootJob, BootJobStore, operation_error};
use httpboot_protocol::{
    DeviceBootJob, DeviceBootStatus, DeviceOperationError, LoaderHardwareInfo,
    MAX_HTTP_BOOT_INITRAMFS_BYTES, MacAddress,
};
use someboot::{
    boot_resources::PhysicalRange,
    handoff::{HandoffSpec, PreparedHandoff},
};

use super::elf_loader;

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
    pub handoff: PreparedHandoff,
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

    pub fn status(&self) -> Option<DeviceBootStatus> {
        self.jobs.status()
    }

    pub fn status_for(&self, id: &str) -> Result<DeviceBootStatus, DeviceOperationError> {
        self.jobs.status_for(id)
    }

    pub fn failure(&self, id: &str) -> Result<Option<&DeviceOperationError>, DeviceOperationError> {
        self.jobs.failure(id)
    }

    pub fn create(&mut self, manifest: DeviceBootJob) -> Result<bool, DeviceOperationError> {
        self.jobs.create(
            manifest,
            MAX_HTTP_BOOT_INITRAMFS_BYTES,
            MAX_HTTP_BOOT_INITRAMFS_BYTES,
        )
    }

    pub fn descriptor(
        &self,
        id: &str,
        kind: FileKind,
    ) -> Result<&httpboot_protocol::DeviceBootImage, DeviceOperationError> {
        self.jobs.descriptor(id, kind)
    }

    pub fn upload(
        &mut self,
        id: &str,
        kind: FileKind,
        data: alloc::vec::Vec<u8>,
    ) -> Result<(), DeviceOperationError> {
        self.jobs.upload(id, kind, data)
    }

    pub fn cancel(&mut self, id: &str) -> Result<(), DeviceOperationError> {
        self.jobs.cancel(id)
    }

    pub fn fail(
        &mut self,
        id: &str,
        error: DeviceOperationError,
    ) -> Result<(), DeviceOperationError> {
        self.jobs.fail(id, error)
    }

    pub fn prepare(
        &mut self,
        id: &str,
        binding: Option<&str>,
    ) -> Result<BootExecution, DeviceOperationError> {
        let job = self.jobs.job(id)?;
        if !binding.is_some_and(|binding| self.serial.borrow().permits_start(binding)) {
            return Err(operation_error(
                "serial_binding_required",
                "a valid X-Serial-Binding header is required before start",
            ));
        }
        if job.kernel().is_none() {
            return Err(operation_error(
                "kernel_not_uploaded",
                "kernel upload is not complete",
            ));
        }
        if job.manifest().initramfs.is_some() && job.initramfs().is_none() {
            return Err(operation_error(
                "initramfs_not_uploaded",
                "initramfs upload is not complete",
            ));
        }
        let prepared = load_execution(job);
        if let Err(error) = &prepared {
            self.jobs.fail(id, error.clone())?;
        }
        prepared
    }
}

fn load_execution(job: &BootJob) -> Result<BootExecution, DeviceOperationError> {
    let bytes = job.kernel().expect("prepare checked kernel presence");
    let elf = elf_loader::load_elf(bytes).map_err(|error| error.device_error())?;
    let handoff = PreparedHandoff::prepare(
        HandoffSpec {
            preserve: &[],
            image: PhysicalRange::new(elf.load_addr, elf.load_end - elf.load_addr),
            cmdline: job.manifest().cmdline.as_deref(),
            initramfs: job.initramfs(),
            metadata: None,
        },
        None,
    )
    .map_err(|error| {
        operation_error(
            "handoff_prepare_failed",
            format!("EFI handoff preparation failed: {error}"),
        )
    })?;
    Ok(BootExecution { elf, handoff })
}
