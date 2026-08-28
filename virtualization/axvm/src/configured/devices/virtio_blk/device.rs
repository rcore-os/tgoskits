//! Configured VirtIO MMIO block device runtime and backend.

#[cfg(feature = "fs")]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "fs")]
use std::collections::VecDeque;
use std::{
    boxed::Box,
    format,
    sync::{Arc, Mutex},
    vec,
    vec::Vec,
};

use axdevice::*;
use axdevice_base::{
    BusKind, Device, DeviceAccess, DeviceContext, DeviceError, DmaGrant, InterruptSharing,
    InterruptTrigger, IrqLine, Resource,
};
use axvirtio_blk::{BlockBackend, BlockDeviceEvent, VirtioBlockConfig, VirtioMmioBlockDevice};
use axvirtio_common::{GuestMemory, NoGuestMemoryAccessor, VirtioError, VirtioResult};
use axvm_types::GuestPhysAddr;
use axvmconfig::VirtualDeviceRequest;

#[cfg(feature = "fs")]
use super::image::{
    AxFilePublisher, ImagePreparationError, allocate_file_mirror, prepare_file_image,
};
use super::options::{BackendConfig, FilesystemFormat, VirtioBlkOptions};
use crate::{ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext};

const MMIO_SLOT: &str = "mmio";
const IRQ_SLOT: &str = "irq";
const MMIO_SIZE: u64 = 0x200;
const SECTOR_SIZE: usize = 512;
const DEFAULT_RAMDISK_CAPACITY_BYTES: u64 = 2 * 1024 * 1024;

/// Catalog entry for `[[devices.virtual]] model = "virtio-blk"`.
pub(super) const REGISTRATION: ConfiguredModelRegistration = ConfiguredModelRegistration {
    model: "virtio-blk",
    create: create_device_node,
};

fn create_device_node(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    let options = VirtioBlkOptions::parse(request)
        .map_err(|error| invalid_options(request, &error.to_string()))?;
    let backend =
        VirtioBlkBackend::open(&options.backend, options.capacity_bytes).map_err(|error| {
            ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: format!("failed to initialize backing storage: {error}"),
            }
        })?;
    let controller =
        context
            .default_wired_controller()
            .ok_or_else(|| ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: "virtio-blk requires a wired interrupt controller".into(),
            })?;
    let model: Arc<dyn DeviceModel> = Arc::new(VirtioBlkModel {
        backend: Mutex::new(Some(backend)),
        controller,
    });
    let mut node = DeviceNodeSpec::virtual_device(id, model);
    if let Some(controller_node) = context.default_wired_controller_node() {
        node = node.with_dependency(controller_node.clone());
    }
    Ok(node)
}

fn invalid_options(request: &VirtualDeviceRequest, detail: &str) -> ConfiguredDeviceError {
    ConfiguredDeviceError::InvalidOptions {
        device: request.id.clone(),
        model: request.model.clone(),
        detail: detail.into(),
    }
}

fn invalid_device_config(operation: &'static str, detail: &str) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation,
        detail: detail.into(),
    }
}

fn allocate_zeroed_backend_buffer(
    capacity: u64,
    operation: &'static str,
) -> DeviceManagerResult<Vec<u8>> {
    let byte_len = usize::try_from(capacity).map_err(|_| {
        invalid_device_config(operation, "capacity does not fit the host address space")
    })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(byte_len).map_err(|_| {
        invalid_device_config(
            operation,
            "capacity cannot be allocated in the host address space",
        )
    })?;
    bytes.resize(byte_len, 0);
    Ok(bytes)
}

struct VirtioBlkModel {
    backend: Mutex<Option<VirtioBlkBackend>>,
    controller: axdevice_base::InterruptControllerId,
}

impl DeviceModel for VirtioBlkModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new(MMIO_SLOT)?,
                MMIO_SIZE,
                MMIO_SIZE,
                ResourceRequest::Auto,
            )?
            .with_wired_irq(
                ResourceSlot::new(IRQ_SLOT)?,
                self.controller,
                InterruptTrigger::EdgeTriggered,
                InterruptSharing::Exclusive,
                ResourceRequest::Auto,
            )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        let registers = ResourceSlot::new(MMIO_SLOT).expect("static slot is valid");
        let interrupt = ResourceSlot::new(IRQ_SLOT).expect("static slot is valid");
        DeviceFirmwareSpec::interfaces(
            Some(std::vec![FdtContributionSpec::Conventional(
                FdtNodeSpec::new("virtio_mmio")
                    .with_compatible("virtio,mmio")
                    .with_register(registers.clone())
                    .with_interrupt(interrupt.clone())
                    .with_empty_property("dma-coherent"),
            )]),
            Some(std::vec![AcpiContributionSpec::Conventional(
                AcpiDeviceSpec::new_indexed("VB", "LNRO0005")
                    .with_register(registers)
                    .with_interrupt(interrupt),
            )]),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, size) = context.mmio(MMIO_SLOT)?;
        let irq = context.irq(IRQ_SLOT)?;
        let irq_id = irq.input().value() as u32;
        let backend = self
            .backend
            .lock()
            .map_err(|_| {
                invalid_device_config(
                    "take virtio-blk backing storage",
                    "backing storage lock is poisoned",
                )
            })?
            .take()
            .ok_or_else(|| {
                invalid_device_config(
                    "take virtio-blk backing storage",
                    "device model was built more than once",
                )
            })?;
        let config = VirtioBlockConfig {
            capacity: backend.capacity_sectors(),
            ..Default::default()
        };
        let model = Arc::new(
            VirtioMmioBlockDevice::new(
                GuestPhysAddr::from(base as usize),
                size as usize,
                backend,
                config,
                NoGuestMemoryAccessor,
            )
            .map_err(|error| DeviceManagerError::InvalidConfig {
                operation: "construct virtio-blk device",
                detail: format!("{error:?}"),
            })?,
        );
        let grant = DmaGrant::new();
        let device = Arc::new(VirtioBlkRuntimeDevice {
            model,
            irq,
            grant: grant.clone(),
            queue_pending: AtomicBool::new(false),
            resources: vec![
                Resource::MmioRange { base, size },
                Resource::IrqLine {
                    line: irq_id,
                    trigger: axdevice_base::InterruptTriggerMode::EdgeTriggered,
                },
            ]
            .into_boxed_slice(),
        });
        let mut bundle = DeviceBundle::new();
        bundle.add_dma_pollable_device(device.clone(), device, grant);
        Ok(bundle)
    }
}

enum VirtioBlkBackend {
    RamDisk(RamDiskBackend),
    #[cfg(feature = "fs")]
    File(FileBackend),
}

impl VirtioBlkBackend {
    fn open(config: &BackendConfig, capacity_bytes: Option<u64>) -> DeviceManagerResult<Self> {
        match config {
            BackendConfig::RamDisk => {
                let capacity = capacity_bytes.unwrap_or(DEFAULT_RAMDISK_CAPACITY_BYTES);
                Ok(Self::RamDisk(RamDiskBackend::new(capacity)?))
            }
            BackendConfig::File { path, filesystem } => {
                open_file_backend(path, capacity_bytes, *filesystem)
            }
        }
    }

    const fn capacity_sectors(&self) -> u64 {
        match self {
            Self::RamDisk(backend) => backend.capacity_sectors,
            #[cfg(feature = "fs")]
            Self::File(backend) => backend.capacity_sectors,
        }
    }
}

#[cfg(feature = "fs")]
fn open_file_backend(
    path: &str,
    configured_capacity: Option<u64>,
    filesystem: FilesystemFormat,
) -> DeviceManagerResult<VirtioBlkBackend> {
    let mut publisher =
        AxFilePublisher::open(path).map_err(|error| map_image_error(path, error))?;
    let bytes = prepare_file_image(
        &mut publisher,
        configured_capacity,
        filesystem,
        allocate_file_mirror,
    )
    .map_err(|error| map_image_error(path, error))?;
    let capacity_sectors = bytes.len() as u64 / SECTOR_SIZE as u64;
    FileBackend::spawn(publisher.into_file(), bytes, capacity_sectors).map(VirtioBlkBackend::File)
}

#[cfg(feature = "fs")]
fn map_image_error(path: &str, error: ImagePreparationError) -> DeviceManagerError {
    invalid_device_config(
        "prepare virtio-blk backing image",
        &format!("failed to prepare `{path}`: {error}"),
    )
}

#[cfg(not(feature = "fs"))]
fn open_file_backend(
    path: &str,
    _configured_capacity: Option<u64>,
    _filesystem: FilesystemFormat,
) -> DeviceManagerResult<VirtioBlkBackend> {
    Err(invalid_device_config(
        "open virtio-blk backing file",
        &format!("file backend `{path}` requires the AxVM `fs` feature"),
    ))
}

impl BlockBackend for VirtioBlkBackend {
    fn requires_deferred_processing(&self) -> bool {
        match self {
            Self::RamDisk(_) => false,
            #[cfg(feature = "fs")]
            Self::File(_) => true,
        }
    }

    fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
        match self {
            Self::RamDisk(backend) => backend.read(sector, buffer),
            #[cfg(feature = "fs")]
            Self::File(backend) => backend.read(sector, buffer),
        }
    }

    fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
        match self {
            Self::RamDisk(backend) => backend.write(sector, buffer),
            #[cfg(feature = "fs")]
            Self::File(backend) => backend.write(sector, buffer),
        }
    }

    fn flush(&self) -> VirtioResult<()> {
        match self {
            Self::RamDisk(backend) => backend.flush(),
            #[cfg(feature = "fs")]
            Self::File(backend) => backend.flush(),
        }
    }
}

struct RamDiskBackend {
    bytes: Mutex<Vec<u8>>,
    capacity_sectors: u64,
}

impl RamDiskBackend {
    fn new(capacity_bytes: u64) -> DeviceManagerResult<Self> {
        Ok(Self {
            bytes: Mutex::new(allocate_zeroed_backend_buffer(
                capacity_bytes,
                "allocate virtio-blk ramdisk",
            )?),
            capacity_sectors: capacity_bytes / SECTOR_SIZE as u64,
        })
    }

    fn range(&self, sector: u64, len: usize) -> VirtioResult<core::ops::Range<usize>> {
        let start = usize::try_from(sector)
            .ok()
            .and_then(|sector| sector.checked_mul(SECTOR_SIZE))
            .ok_or(VirtioError::InvalidAddress)?;
        let end = start.checked_add(len).ok_or(VirtioError::InvalidAddress)?;
        if end
            > self
                .bytes
                .lock()
                .expect("virtio-blk ramdisk mutex poisoned")
                .len()
        {
            return Err(VirtioError::InvalidAddress);
        }
        Ok(start..end)
    }
}

#[cfg(feature = "fs")]
struct FileBackend {
    bytes: Mutex<Vec<u8>>,
    writes: Arc<Mutex<VecDeque<FileWrite>>>,
    pending_writes: Arc<AtomicUsize>,
    writeback_failed: Arc<AtomicBool>,
    stop_worker: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    capacity_sectors: u64,
}

#[cfg(feature = "fs")]
struct FileWrite {
    offset: u64,
    bytes: Vec<u8>,
}

#[cfg(feature = "fs")]
impl FileBackend {
    fn spawn(
        file: ax_api::fs::AxFileHandle,
        bytes: Vec<u8>,
        capacity_sectors: u64,
    ) -> DeviceManagerResult<Self> {
        let writes = Arc::new(Mutex::new(VecDeque::<FileWrite>::new()));
        let worker_writes = writes.clone();
        let pending_writes = Arc::new(AtomicUsize::new(0));
        let worker_pending = pending_writes.clone();
        let writeback_failed = Arc::new(AtomicBool::new(false));
        let worker_failed = writeback_failed.clone();
        let stop_worker = Arc::new(AtomicBool::new(false));
        let worker_stop = stop_worker.clone();
        let worker = std::thread::Builder::new()
            .name("virtio-blk-file".into())
            .spawn(move || {
                loop {
                    if let Some(write) = worker_writes
                        .lock()
                        .expect("virtio-blk file queue mutex poisoned")
                        .pop_front()
                    {
                        match ax_api::fs::ax_write_file_at(&file, write.offset, &write.bytes) {
                            Ok(written) if written == write.bytes.len() => {
                                if let Err(error) = ax_api::fs::ax_flush_file(&file) {
                                    error!("virtio-blk file write-back flush failed: {error}");
                                    worker_failed.store(true, Ordering::Release);
                                }
                            }
                            Ok(written) => {
                                error!(
                                    "virtio-blk file write-back was short: wrote {written} of {} \
                                     bytes",
                                    write.bytes.len()
                                );
                                worker_failed.store(true, Ordering::Release);
                            }
                            Err(error) => {
                                error!("virtio-blk file write-back failed: {error}");
                                worker_failed.store(true, Ordering::Release);
                            }
                        }
                        worker_pending.fetch_sub(1, Ordering::AcqRel);
                    } else if worker_stop.load(Ordering::Acquire) {
                        break;
                    } else {
                        std::thread::sleep(core::time::Duration::from_millis(1));
                    }
                }
            })
            .map_err(|error| {
                invalid_device_config(
                    "spawn virtio-blk file worker",
                    &format!("failed to spawn worker: {error}"),
                )
            })?;
        Ok(Self {
            bytes: Mutex::new(bytes),
            writes,
            pending_writes,
            writeback_failed,
            stop_worker,
            worker: Some(worker),
            capacity_sectors,
        })
    }

    fn range(&self, sector: u64, len: usize) -> VirtioResult<core::ops::Range<usize>> {
        let start = usize::try_from(sector)
            .ok()
            .and_then(|sector| sector.checked_mul(SECTOR_SIZE))
            .ok_or(VirtioError::InvalidAddress)?;
        let end = start.checked_add(len).ok_or(VirtioError::InvalidAddress)?;
        if end
            > self
                .bytes
                .lock()
                .expect("virtio-blk file mirror mutex poisoned")
                .len()
        {
            return Err(VirtioError::InvalidAddress);
        }
        Ok(start..end)
    }
}

#[cfg(feature = "fs")]
impl Drop for FileBackend {
    fn drop(&mut self) {
        // The worker drains every queued write before observing this flag, so
        // teardown never silently discards writes already accepted from the guest.
        self.stop_worker.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            error!("virtio-blk file worker failed during teardown");
        }
    }
}

#[cfg(feature = "fs")]
impl BlockBackend for FileBackend {
    fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
        let range = self.range(sector, buffer.len())?;
        buffer.copy_from_slice(
            &self
                .bytes
                .lock()
                .expect("virtio-blk file mirror mutex poisoned")[range],
        );
        Ok(buffer.len())
    }

    fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
        let range = self.range(sector, buffer.len())?;
        self.bytes
            .lock()
            .expect("virtio-blk file mirror mutex poisoned")[range.clone()]
        .copy_from_slice(buffer);
        self.pending_writes.fetch_add(1, Ordering::AcqRel);
        self.writes
            .lock()
            .expect("virtio-blk file queue mutex poisoned")
            .push_back(FileWrite {
                offset: range.start as u64,
                bytes: buffer.to_vec(),
            });
        Ok(buffer.len())
    }

    fn flush(&self) -> VirtioResult<()> {
        if self.pending_writes.load(Ordering::Acquire) != 0 {
            return Err(VirtioError::WouldBlock);
        }
        if self.writeback_failed.load(Ordering::Acquire) {
            return Err(VirtioError::BackendError);
        }
        Ok(())
    }
}

impl BlockBackend for RamDiskBackend {
    fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
        let range = self.range(sector, buffer.len())?;
        buffer.copy_from_slice(
            &self
                .bytes
                .lock()
                .expect("virtio-blk ramdisk mutex poisoned")[range],
        );
        Ok(buffer.len())
    }

    fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
        let range = self.range(sector, buffer.len())?;
        self.bytes
            .lock()
            .expect("virtio-blk ramdisk mutex poisoned")[range]
            .copy_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&self) -> VirtioResult<()> {
        Ok(())
    }
}

struct ScopedDeviceMemory<'a> {
    context: &'a mut dyn DeviceContext,
    grant: &'a DmaGrant,
}

impl GuestMemory for ScopedDeviceMemory<'_> {
    fn read(&mut self, guest_addr: GuestPhysAddr, data: &mut [u8]) -> VirtioResult<()> {
        self.context
            .read_guest_memory(self.grant, guest_addr, data)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    fn write(&mut self, guest_addr: GuestPhysAddr, data: &[u8]) -> VirtioResult<()> {
        self.context
            .write_guest_memory(self.grant, guest_addr, data)
            .map_err(|_| VirtioError::InvalidAddress)
    }
}

struct VirtioBlkRuntimeDevice {
    model: Arc<VirtioMmioBlockDevice<VirtioBlkBackend, NoGuestMemoryAccessor>>,
    irq: IrqLine,
    grant: DmaGrant,
    queue_pending: AtomicBool,
    resources: Box<[Resource]>,
}

impl Device for VirtioBlkRuntimeDevice {
    fn name(&self) -> &str {
        "virtio-blk"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(
        &self,
        access: &DeviceAccess,
        _context: &mut dyn DeviceContext,
    ) -> Result<u64, DeviceError> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        self.model
            .mmio_read(
                GuestPhysAddr::from(access.address() as usize),
                access.width(),
            )
            .map(|value| value as u64)
            .map_err(map_virtio_error)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> Result<(), DeviceError> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        let mut memory = ScopedDeviceMemory {
            context,
            grant: &self.grant,
        };
        let event = self
            .model
            .mmio_write_with_memory(
                GuestPhysAddr::from(access.address() as usize),
                access.width(),
                value as usize,
                &mut memory,
            )
            .map_err(map_virtio_error)?;
        match event {
            BlockDeviceEvent::InterruptPending => {
                self.irq.pulse().map_err(|error| DeviceError::Backend {
                    operation: "pulse virtio-blk interrupt",
                    detail: format!("{error}"),
                })?
            }
            BlockDeviceEvent::QueuePending(0) => self.queue_pending.store(true, Ordering::Release),
            BlockDeviceEvent::QueuePending(_) => {
                return Err(DeviceError::InvalidInput {
                    operation: "notify virtio-blk queue",
                    detail: "only queue 0 is supported".into(),
                });
            }
            BlockDeviceEvent::Reset => {
                self.queue_pending.store(false, Ordering::Release);
            }
            BlockDeviceEvent::None => {}
        }
        Ok(())
    }
}

impl DmaPollableDeviceOps for VirtioBlkRuntimeDevice {
    fn poll_dma(
        &self,
        _now_ns: u64,
        context: &mut dyn DeviceContext,
        grant: &DmaGrant,
    ) -> DeviceManagerResult {
        if !self.queue_pending.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let mut memory = ScopedDeviceMemory { context, grant };
        let event = self
            .model
            .process_pending_queue(0, &mut memory)
            .map_err(|error| DeviceManagerError::InvalidState {
                operation: "process deferred virtio-blk queue",
                detail: format!("{error:?}"),
            })?;
        match event {
            BlockDeviceEvent::InterruptPending => {
                self.irq
                    .pulse()
                    .map_err(|error| DeviceManagerError::InvalidState {
                        operation: "pulse deferred virtio-blk interrupt",
                        detail: format!("{error}"),
                    })?;
            }
            BlockDeviceEvent::QueuePending(0) => {
                self.queue_pending.store(true, Ordering::Release);
            }
            BlockDeviceEvent::QueuePending(_) => {
                return Err(DeviceManagerError::InvalidState {
                    operation: "process deferred virtio-blk queue",
                    detail: "only queue 0 is supported".into(),
                });
            }
            BlockDeviceEvent::None | BlockDeviceEvent::Reset => {}
        }
        Ok(())
    }
}

fn map_virtio_error(error: VirtioError) -> DeviceError {
    DeviceError::InvalidInput {
        operation: "access virtio-blk MMIO transport",
        detail: format!("{error:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::allocate_zeroed_backend_buffer;

    #[test]
    fn oversized_backend_capacity_returns_configuration_error() {
        assert!(allocate_zeroed_backend_buffer(u64::MAX, "test virtio-blk allocation").is_err());
    }
}
