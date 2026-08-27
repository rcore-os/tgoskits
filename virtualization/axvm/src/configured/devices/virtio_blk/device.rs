//! Configured VirtIO MMIO block device backed by memory or a file.

use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "fs")]
use core::sync::atomic::{AtomicU8, AtomicUsize};
use std::{
    boxed::Box,
    format,
    sync::{Arc, Mutex},
    vec,
    vec::Vec,
};
#[cfg(feature = "fs")]
use std::{collections::VecDeque, string::String};

use axdevice::*;
use axdevice_base::{
    BusKind, Device, DeviceAccess, DeviceContext, DeviceError, DmaGrant, InterruptSharing,
    InterruptTrigger, IrqLine, IrqResult, Resource,
};
use axvirtio_blk::{BlockBackend, BlockDeviceEvent, VirtioBlockConfig, VirtioMmioBlockDevice};
use axvirtio_common::{GuestMemory, NoGuestMemoryAccessor, VirtioError, VirtioResult};
use axvm_types::GuestPhysAddr;
use axvmconfig::VirtualDeviceRequest;

use super::options::{BackendConfig, parse_backend};
#[cfg(feature = "fs")]
use super::{
    image::{ImagePreparationError, ImagePublisher, prepare_file_image},
    options::FilesystemFormat,
};
use crate::{ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext};

const MMIO_SLOT: &str = "mmio";
const IRQ_SLOT: &str = "irq";
const MMIO_SIZE: u64 = 0x200;
const SECTOR_SIZE: usize = 512;
#[cfg(feature = "fs")]
const FILE_WRITE_CHUNK_SIZE: usize = 4096;
#[cfg(feature = "fs")]
const FILE_WORKER_STACK_SIZE: usize = 1024 * 1024;
#[cfg(feature = "fs")]
const FILE_FLUSH_IDLE: u8 = 0;
#[cfg(feature = "fs")]
const FILE_FLUSH_PENDING: u8 = 1;
#[cfg(feature = "fs")]
const FILE_FLUSH_COMPLETE: u8 = 2;
const DEFAULT_CAPACITY_BYTES: u64 = 2 * 1024 * 1024;
#[cfg(feature = "fs")]
const DEFAULT_EXT4_CAPACITY_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const VIRTIO_BLK_IRQ_TRIGGER: InterruptTrigger = InterruptTrigger::LevelTriggered;

const fn interrupt_line_should_be_asserted(interrupt_status: u32) -> bool {
    interrupt_status != 0
}

fn synchronize_interrupt_line(irq: &IrqLine, interrupt_status: u32) -> IrqResult {
    if interrupt_line_should_be_asserted(interrupt_status) {
        irq.assert()
    } else {
        irq.deassert()
    }
}

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
    let backend_config =
        parse_backend(request).map_err(|detail| invalid_options(request, detail))?;
    let capacity_bytes =
        parse_capacity(request).map_err(|detail| invalid_options(request, detail))?;
    let vm_id = match &backend_config {
        BackendConfig::RamDisk => None,
        BackendConfig::File { .. } => {
            Some(
                context
                    .vm_id()
                    .ok_or_else(|| ConfiguredDeviceError::Instantiation {
                        device: request.id.clone(),
                        model: request.model.clone(),
                        detail: "virtio-blk file backend requires a VM identity".into(),
                    })?,
            )
        }
    };
    let backend = VirtioBlkBackend::open(request, &backend_config, capacity_bytes, vm_id).map_err(
        |error| ConfiguredDeviceError::Instantiation {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: format!("failed to initialize backing storage: {error}"),
        },
    )?;
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

fn parse_capacity(request: &VirtualDeviceRequest) -> Result<Option<u64>, &'static str> {
    let capacity = request.options.get("capacity");
    let legacy_sectors = request.options.get("capacity_sectors");
    if capacity.is_some() && legacy_sectors.is_some() {
        return Err("specify only one of `capacity` and `capacity_sectors`");
    }
    let bytes = if let Some(value) = capacity {
        let value = value.as_str().ok_or("`capacity` must be a size string")?;
        Some(parse_capacity_bytes(value)?)
    } else if let Some(value) = legacy_sectors {
        let sectors = value
            .as_integer()
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or("`capacity_sectors` must be positive")?;
        Some(
            sectors
                .checked_mul(SECTOR_SIZE as u64)
                .ok_or("`capacity_sectors` is too large")?,
        )
    } else {
        None
    };
    Ok(bytes)
}

fn parse_capacity_bytes(value: &str) -> Result<u64, &'static str> {
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(split);
    let number = number
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or("`capacity` must start with a positive integer")?;
    let multiplier = match suffix.to_ascii_lowercase().as_str() {
        "b" => 1,
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "kib" => 1024,
        "mib" => 1024 * 1024,
        "gib" => 1024 * 1024 * 1024,
        _ => return Err("`capacity` suffix must be B, KB, MB, GB, KiB, MiB, or GiB"),
    };
    let bytes = number
        .checked_mul(multiplier)
        .ok_or("`capacity` is too large")?;
    if bytes % SECTOR_SIZE as u64 != 0 {
        return Err("`capacity` must be a multiple of 512 bytes");
    }
    Ok(bytes)
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
                VIRTIO_BLK_IRQ_TRIGGER,
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
        let resolved_irq = irq.input().value();
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
            resources: runtime_resources(base, size, resolved_irq),
        });
        let mut bundle = DeviceBundle::new();
        bundle.add_dma_pollable_device(device.clone(), device, grant);
        Ok(bundle)
    }
}

fn runtime_resources(base: u64, size: u64, resolved_irq: usize) -> Box<[Resource]> {
    vec![
        Resource::MmioRange { base, size },
        Resource::IrqLine {
            line: resolved_irq as u32,
            trigger: axdevice_base::InterruptTriggerMode::LevelTriggered,
        },
    ]
    .into_boxed_slice()
}

enum VirtioBlkBackend {
    RamDisk(RamDiskBackend),
    #[cfg(feature = "fs")]
    File(FileBackend),
}

impl VirtioBlkBackend {
    fn open(
        request: &VirtualDeviceRequest,
        config: &BackendConfig,
        capacity_bytes: Option<u64>,
        vm_id: Option<usize>,
    ) -> DeviceManagerResult<Self> {
        match config {
            BackendConfig::RamDisk => {
                let capacity = capacity_bytes.unwrap_or(DEFAULT_CAPACITY_BYTES);
                Ok(Self::RamDisk(RamDiskBackend::new(capacity)?))
            }
            BackendConfig::File { path, .. } => {
                let vm_id = vm_id.ok_or_else(|| {
                    invalid_device_config(
                        "open virtio-blk backing file",
                        "file backend requires a VM identity",
                    )
                })?;
                open_file_backend(request, path, vm_id)
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
    request: &VirtualDeviceRequest,
    path: &str,
    vm_id: usize,
) -> DeviceManagerResult<VirtioBlkBackend> {
    let mut options = ax_api::fs::AxOpenOptions::new();
    options.read(true);
    options.write(true);
    options.create(true);
    options.direct(true);
    let file = ax_api::fs::ax_open_file(path, &options).map_err(|error| {
        invalid_device_config(
            "open virtio-blk backing file",
            &format!("failed to open `{path}`: {error}"),
        )
    })?;
    let mut publisher = AxFilePublisher { file: &file };
    let bytes = prepare_file_backend_image(
        request,
        &mut publisher,
        DEFAULT_CAPACITY_BYTES,
        |capacity| {
            allocate_file_mirror(capacity).map_err(|error| {
                ImagePreparationError::new("allocate backing file mirror", format!("{error}"))
            })
        },
    )
    .map_err(|error| {
        invalid_device_config(
            "prepare virtio-blk backing file",
            &format!("failed to prepare `{path}`: {error}"),
        )
    })?;
    let capacity = bytes.len() as u64;
    FileBackend::new(file, bytes, capacity / SECTOR_SIZE as u64, vm_id).map(VirtioBlkBackend::File)
}

#[cfg(feature = "fs")]
pub(crate) fn prepare_file_backend_image<P, A>(
    request: &VirtualDeviceRequest,
    publisher: &mut P,
    default_capacity: u64,
    allocate: A,
) -> Result<Vec<u8>, ImagePreparationError>
where
    P: ImagePublisher,
    A: FnMut(u64) -> Result<Vec<u8>, ImagePreparationError>,
{
    let configured_capacity = parse_capacity(request)
        .map_err(|detail| ImagePreparationError::new("parse virtio-blk options", detail))?;
    let filesystem = match parse_backend(request)
        .map_err(|detail| ImagePreparationError::new("parse virtio-blk options", detail))?
    {
        BackendConfig::File { filesystem, .. } => filesystem,
        BackendConfig::RamDisk => {
            return Err(ImagePreparationError::new(
                "parse virtio-blk options",
                "file image preparation requires the file backend",
            ));
        }
    };
    let default_capacity = match (configured_capacity, filesystem) {
        (None, FilesystemFormat::Ext4) => DEFAULT_EXT4_CAPACITY_BYTES,
        _ => default_capacity,
    };

    prepare_file_image(
        publisher,
        configured_capacity,
        default_capacity,
        filesystem,
        allocate,
    )
}

#[cfg(feature = "fs")]
struct AxFilePublisher<'a> {
    file: &'a ax_api::fs::AxFileHandle,
}

#[cfg(feature = "fs")]
impl ImagePublisher for AxFilePublisher<'_> {
    fn len(&self) -> Result<u64, String> {
        ax_api::fs::ax_file_attr(self.file)
            .map(|attribute| attribute.size)
            .map_err(|error| format!("{error}"))
    }

    fn resize(&mut self, len: u64) -> Result<(), String> {
        ax_api::fs::ax_truncate_file(self.file, len).map_err(|error| format!("{error}"))
    }

    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, String> {
        ax_api::fs::ax_read_file_at(self.file, offset, bytes).map_err(|error| format!("{error}"))
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<usize, String> {
        ax_api::fs::ax_write_file_at(self.file, offset, bytes).map_err(|error| format!("{error}"))
    }

    fn flush(&mut self) -> Result<(), String> {
        ax_api::fs::ax_flush_file(self.file).map_err(|error| format!("{error}"))
    }
}

#[cfg(feature = "fs")]
fn allocate_file_mirror(capacity: u64) -> DeviceManagerResult<Vec<u8>> {
    allocate_zeroed_backend_buffer(capacity, "allocate virtio-blk file mirror")
}

#[cfg(not(feature = "fs"))]
fn open_file_backend(
    _request: &VirtualDeviceRequest,
    path: &str,
    _vm_id: usize,
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
    operations: Arc<Mutex<VecDeque<FileOperation>>>,
    pending_writes: Arc<AtomicUsize>,
    flush_state: Arc<AtomicU8>,
    writeback_failed: Arc<AtomicBool>,
    stop_worker: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    capacity_sectors: u64,
}

#[cfg(feature = "fs")]
enum FileOperation {
    Write { offset: u64, bytes: Vec<u8> },
    Flush,
}

#[cfg(feature = "fs")]
impl FileBackend {
    fn new(
        file: ax_api::fs::AxFileHandle,
        bytes: Vec<u8>,
        capacity_sectors: u64,
        vm_id: usize,
    ) -> DeviceManagerResult<Self> {
        let operations = Arc::new(Mutex::new(VecDeque::<FileOperation>::new()));
        let worker_operations = operations.clone();
        let pending_writes = Arc::new(AtomicUsize::new(0));
        let worker_pending = pending_writes.clone();
        let flush_state = Arc::new(AtomicU8::new(FILE_FLUSH_IDLE));
        let worker_flush_state = flush_state.clone();
        let writeback_failed = Arc::new(AtomicBool::new(false));
        let worker_failed = writeback_failed.clone();
        let stop_worker = Arc::new(AtomicBool::new(false));
        let worker_stop = stop_worker.clone();
        let worker = std::thread::Builder::new()
            .name("virtio-blk-file".into())
            .stack_size(FILE_WORKER_STACK_SIZE)
            .spawn(move || {
                let worker_cpu = crate::host::cpu::current_id();
                if ax_api::task::ax_set_current_affinity(ax_api::task::AxCpuMask::one_shot(
                    worker_cpu,
                ))
                .is_err()
                {
                    warn!("failed to pin virtio-blk file worker to CPU{worker_cpu}");
                }
                loop {
                    if let Some(operation) = worker_operations
                        .lock()
                        .expect("virtio-blk file queue mutex poisoned")
                        .pop_front()
                    {
                        match operation {
                            FileOperation::Write { offset, bytes } => {
                                if write_file_in_chunks(&file, offset, &bytes).is_err() {
                                    error!("virtio-blk file write-back failed");
                                    worker_failed.store(true, Ordering::Release);
                                }
                                let previous_pending =
                                    worker_pending.fetch_sub(1, Ordering::AcqRel);
                                if previous_pending == 1 {
                                    notify_file_backend_progress(vm_id);
                                }
                            }
                            FileOperation::Flush => {
                                if let Err(error) = ax_api::fs::ax_flush_file(&file) {
                                    error!("virtio-blk file write-back flush failed: {error}");
                                    worker_failed.store(true, Ordering::Release);
                                }
                                worker_flush_state.store(FILE_FLUSH_COMPLETE, Ordering::Release);
                                notify_file_backend_progress(vm_id);
                            }
                        }
                    } else if worker_stop.load(Ordering::Acquire) {
                        break;
                    } else {
                        std::thread::park();
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
            operations,
            pending_writes,
            flush_state,
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

    fn wake_worker(&self) {
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }
}

#[cfg(feature = "fs")]
fn notify_file_backend_progress(vm_id: usize) {
    if let Err(error) = crate::AxvmRuntime::notify_vm(vm_id) {
        warn!("failed to publish VM[{vm_id}] virtio-blk device work: {error}");
    }
    if let Err(error) = crate::notify_vm_vcpu(vm_id, 0) {
        warn!("failed to wake VM[{vm_id}] after virtio-blk file progress: {error}");
    }
}

#[cfg(feature = "fs")]
fn write_file_in_chunks(
    file: &ax_api::fs::AxFileHandle,
    offset: u64,
    bytes: &[u8],
) -> Result<(), ()> {
    let mut written = 0;
    while written < bytes.len() {
        let end = (written + FILE_WRITE_CHUNK_SIZE).min(bytes.len());
        let count =
            ax_api::fs::ax_write_file_at(file, offset + written as u64, &bytes[written..end])
                .map_err(|_| ())?;
        if count == 0 || count > end - written {
            return Err(());
        }
        written += count;
    }
    Ok(())
}

#[cfg(feature = "fs")]
impl Drop for FileBackend {
    fn drop(&mut self) {
        self.stop_worker.store(true, Ordering::Release);
        self.wake_worker();
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
        self.operations
            .lock()
            .expect("virtio-blk file queue mutex poisoned")
            .push_back(FileOperation::Write {
                offset: range.start as u64,
                bytes: buffer.to_vec(),
            });
        self.wake_worker();
        Ok(buffer.len())
    }

    fn flush(&self) -> VirtioResult<()> {
        if self.writeback_failed.load(Ordering::Acquire) {
            return Err(VirtioError::BackendError);
        }
        if self.pending_writes.load(Ordering::Acquire) != 0 {
            return Err(VirtioError::WouldBlock);
        }
        match self.flush_state.compare_exchange(
            FILE_FLUSH_IDLE,
            FILE_FLUSH_PENDING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.operations
                    .lock()
                    .expect("virtio-blk file queue mutex poisoned")
                    .push_back(FileOperation::Flush);
                self.wake_worker();
                Err(VirtioError::WouldBlock)
            }
            Err(FILE_FLUSH_PENDING) => Err(VirtioError::WouldBlock),
            Err(FILE_FLUSH_COMPLETE) => {
                self.flush_state.store(FILE_FLUSH_IDLE, Ordering::Release);
                if self.writeback_failed.load(Ordering::Acquire) {
                    Err(VirtioError::BackendError)
                } else {
                    Ok(())
                }
            }
            Err(_) => Err(VirtioError::BackendError),
        }
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
            BlockDeviceEvent::InterruptPending => {}
            BlockDeviceEvent::QueuePending(0) => {
                self.queue_pending.store(true, Ordering::Release);
            }
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
        synchronize_interrupt_line(&self.irq, self.model.interrupt_status()).map_err(|error| {
            DeviceError::Backend {
                operation: "synchronize virtio-blk interrupt",
                detail: format!("{error}"),
            }
        })?;
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
            BlockDeviceEvent::InterruptPending => {}
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
        synchronize_interrupt_line(&self.irq, self.model.interrupt_status()).map_err(|error| {
            DeviceManagerError::InvalidState {
                operation: "synchronize deferred virtio-blk interrupt",
                detail: format!("{error}"),
            }
        })?;
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
    use axdevice::DeviceNodeId;
    use axdevice_base::{InterruptControllerId, InterruptTrigger, Resource};
    use axvmconfig::VirtualDeviceRequest;

    use super::{
        super::register, VIRTIO_BLK_IRQ_TRIGGER, allocate_zeroed_backend_buffer,
        interrupt_line_should_be_asserted, parse_capacity_bytes, runtime_resources,
    };
    use crate::{ConfiguredDeviceCatalog, DeviceInstantiationContext};

    #[test]
    fn ramdisk_catalog_instantiation_does_not_require_vm_id() {
        let mut request = virtual_device_request();
        request
            .options
            .insert("backend".into(), toml::Value::String("ramdisk".into()));

        assert!(
            registered_catalog()
                .instantiate_node(&request, &context_without_vm_id())
                .is_ok()
        );
    }

    #[test]
    fn file_catalog_instantiation_requires_vm_id() {
        let error = match registered_catalog()
            .instantiate_node(&virtual_device_request(), &context_without_vm_id())
        {
            Err(error) => error,
            Ok(_) => panic!("file backend must require a VM identity"),
        };

        assert!(matches!(
            error,
            crate::ConfiguredDeviceError::Instantiation { detail, .. }
                if detail.contains("requires a VM identity")
        ));
    }

    #[test]
    fn virtio_blk_interrupt_is_level_triggered() {
        assert_eq!(
            VIRTIO_BLK_IRQ_TRIGGER,
            InterruptTrigger::LevelTriggered,
            "VirtIO MMIO interrupt status remains pending until the driver acknowledges it"
        );
    }

    #[test]
    fn virtio_blk_interrupt_line_tracks_pending_status() {
        assert!(!interrupt_line_should_be_asserted(0));
        assert!(interrupt_line_should_be_asserted(1));
        assert!(interrupt_line_should_be_asserted(u32::MAX));
    }

    #[test]
    fn runtime_inventory_uses_resolved_irq() {
        let resources = runtime_resources(0x8000_0000, 0x200, 7);
        assert!(matches!(resources[1], Resource::IrqLine { line: 7, .. }));
    }

    #[test]
    fn parses_decimal_and_binary_capacity_suffixes() {
        assert_eq!(parse_capacity_bytes("64MB"), Ok(64_000_000));
        assert_eq!(parse_capacity_bytes("2GB"), Ok(2_000_000_000));
        assert_eq!(parse_capacity_bytes("2MiB"), Ok(2 * 1024 * 1024));
    }

    #[test]
    fn rejects_invalid_or_unaligned_capacity() {
        assert!(parse_capacity_bytes("0GB").is_err());
        assert!(parse_capacity_bytes("1XB").is_err());
        assert!(parse_capacity_bytes("1KB").is_err());
    }

    #[test]
    fn oversized_backend_capacity_returns_configuration_error() {
        assert!(allocate_zeroed_backend_buffer(u64::MAX, "test virtio-blk allocation").is_err());
    }

    fn registered_catalog() -> ConfiguredDeviceCatalog {
        let mut catalog = ConfiguredDeviceCatalog::new();
        register(&mut catalog).expect("register virtio-blk model");
        catalog
    }

    fn context_without_vm_id() -> DeviceInstantiationContext {
        DeviceInstantiationContext::new().with_default_wired_controller(
            DeviceNodeId::new("controller").expect("valid controller node ID"),
            InterruptControllerId::new(0),
        )
    }

    fn virtual_device_request() -> VirtualDeviceRequest {
        VirtualDeviceRequest {
            id: "disk0".into(),
            model: "virtio-blk".into(),
            options: Default::default(),
        }
    }
}
