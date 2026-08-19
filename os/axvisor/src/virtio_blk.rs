//! Configured VirtIO MMIO block device backed by memory or a file.

use alloc::{collections::VecDeque, format, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use axdevice::*;
use axdevice_base::{
    BusKind, Device, DeviceAccess, DeviceContext, DeviceError, DmaGrant, InterruptSharing,
    InterruptTrigger, IrqLine, Resource,
};
use axvirtio_blk::{BlockBackend, BlockDeviceEvent, VirtioBlockConfig, VirtioMmioBlockDevice};
use axvirtio_common::{GuestMemory, NoGuestMemoryAccessor, VirtioError, VirtioResult};
use axvm::{ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext};
use axvm_types::GuestPhysAddr;
use axvmconfig::VirtualDeviceRequest;

const MMIO_SLOT: &str = "mmio";
const IRQ_SLOT: &str = "irq";
const MMIO_BASE: u64 = 0x0a00_0200;
const MMIO_SIZE: u64 = 0x200;
const IRQ_INPUT: usize = 49;
const SECTOR_SIZE: usize = 512;
const DEFAULT_CAPACITY_BYTES: u64 = 2 * 1024 * 1024;

/// Catalog entry for `[[devices.virtual]] model = "virtio-blk"`.
pub const REGISTRATION: ConfiguredModelRegistration = ConfiguredModelRegistration {
    model: "virtio-blk",
    create: create_device_node,
    default_fixed_resources: None,
};

fn create_device_node(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    let capacity_bytes = parse_capacity(request)?;
    let backend_config = parse_backend(request)?;
    let backend = VirtioBlkBackend::open(&backend_config, capacity_bytes).map_err(|error| {
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

fn parse_capacity(request: &VirtualDeviceRequest) -> Result<Option<u64>, ConfiguredDeviceError> {
    let capacity = request.options.get("capacity");
    let legacy_sectors = request.options.get("capacity_sectors");
    if capacity.is_some() && legacy_sectors.is_some() {
        return Err(invalid_options(
            request,
            "specify only one of `capacity` and `capacity_sectors`",
        ));
    }
    let bytes = if let Some(value) = capacity {
        let value = value
            .as_str()
            .ok_or_else(|| invalid_options(request, "`capacity` must be a size string"))?;
        Some(parse_capacity_bytes(value).map_err(|detail| invalid_options(request, detail))?)
    } else if let Some(value) = legacy_sectors {
        let sectors = value
            .as_integer()
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid_options(request, "`capacity_sectors` must be positive"))?;
        Some(
            sectors
                .checked_mul(SECTOR_SIZE as u64)
                .ok_or_else(|| invalid_options(request, "`capacity_sectors` is too large"))?,
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

fn parse_backend(request: &VirtualDeviceRequest) -> Result<BackendConfig, ConfiguredDeviceError> {
    let backend = request
        .options
        .get("backend")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| invalid_options(request, "`backend` must be `file` or `ramdisk`"))
        })
        .transpose()?
        .unwrap_or("file");
    let path = request.options.get("path").map(|value| {
        value
            .as_str()
            .filter(|path| !path.is_empty())
            .ok_or_else(|| invalid_options(request, "`path` must be a non-empty string"))
    });
    match backend {
        "ramdisk" if path.is_none() => Ok(BackendConfig::RamDisk),
        "ramdisk" => Err(invalid_options(
            request,
            "`path` is only valid for the file backend",
        )),
        "file" => Ok(BackendConfig::File {
            path: path
                .transpose()?
                .map(str::to_owned)
                .unwrap_or_else(|| format!("/tmp/{}.img", request.id)),
        }),
        _ => Err(invalid_options(
            request,
            "`backend` must be `file` or `ramdisk`",
        )),
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

enum BackendConfig {
    RamDisk,
    File { path: String },
}

impl DeviceModel for VirtioBlkModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new(MMIO_SLOT)?,
                MMIO_SIZE,
                4,
                ResourceRequest::Fixed(MMIO_BASE),
            )?
            .with_wired_irq(
                ResourceSlot::new(IRQ_SLOT)?,
                self.controller,
                InterruptTrigger::EdgeTriggered,
                InterruptSharing::Exclusive,
                ResourceRequest::Fixed(axdevice_base::ControllerInputId::new(IRQ_INPUT)),
            )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::new("virtio_mmio")
            .with_compatible("virtio,mmio")
            .with_register(ResourceSlot::new(MMIO_SLOT).expect("static slot is valid"))
            .with_interrupt(ResourceSlot::new(IRQ_SLOT).expect("static slot is valid"))
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, size) = context.mmio(MMIO_SLOT)?;
        let irq = context.irq(IRQ_SLOT)?;
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
        let mut config = VirtioBlockConfig::default();
        config.capacity = backend.capacity_sectors();
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
                    line: IRQ_INPUT as u32,
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
                let capacity = capacity_bytes.unwrap_or(DEFAULT_CAPACITY_BYTES);
                Ok(Self::RamDisk(RamDiskBackend::new(capacity)?))
            }
            BackendConfig::File { path } => open_file_backend(path, capacity_bytes),
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
) -> DeviceManagerResult<VirtioBlkBackend> {
    let mut options = ax_api::fs::AxOpenOptions::new();
    options.read(true);
    options.write(true);
    options.create(true);
    let file = ax_api::fs::ax_open_file(path, &options).map_err(|error| {
        invalid_device_config(
            "open virtio-blk backing file",
            &format!("failed to open `{path}`: {error}"),
        )
    })?;
    let existing_len = ax_api::fs::ax_file_attr(&file)
        .map_err(|error| {
            invalid_device_config(
                "inspect virtio-blk backing file",
                &format!("failed to inspect `{path}`: {error}"),
            )
        })?
        .size;
    let capacity = configured_capacity.unwrap_or_else(|| {
        if existing_len == 0 {
            DEFAULT_CAPACITY_BYTES
        } else {
            existing_len
        }
    });
    if capacity == 0 || capacity % SECTOR_SIZE as u64 != 0 {
        return Err(invalid_device_config(
            "validate virtio-blk backing file",
            "backing file capacity must be a positive multiple of 512 bytes",
        ));
    }
    if existing_len != capacity {
        ax_api::fs::ax_truncate_file(&file, capacity).map_err(|error| {
            invalid_device_config(
                "resize virtio-blk backing file",
                &format!("failed to resize `{path}` to {capacity} bytes: {error}"),
            )
        })?;
    }
    let mut bytes = allocate_file_mirror(capacity)?;
    let read = ax_api::fs::ax_read_file_at(&file, 0, &mut bytes).map_err(|error| {
        invalid_device_config(
            "load virtio-blk backing file",
            &format!("failed to read `{path}`: {error}"),
        )
    })?;
    if read != bytes.len() {
        return Err(invalid_device_config(
            "load virtio-blk backing file",
            "backing file returned a short read",
        ));
    }
    FileBackend::spawn(file, bytes, capacity / SECTOR_SIZE as u64).map(VirtioBlkBackend::File)
}

#[cfg(feature = "fs")]
fn allocate_file_mirror(capacity: u64) -> DeviceManagerResult<Vec<u8>> {
    allocate_zeroed_backend_buffer(capacity, "allocate virtio-blk file mirror")
}

#[cfg(not(feature = "fs"))]
fn open_file_backend(
    path: &str,
    _configured_capacity: Option<u64>,
) -> DeviceManagerResult<VirtioBlkBackend> {
    Err(invalid_device_config(
        "open virtio-blk backing file",
        &format!("file backend `{path}` requires the AxVisor `fs` feature"),
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
                                    "virtio-blk file write-back was short: wrote {written} of {} bytes",
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
    resources: alloc::boxed::Box<[Resource]>,
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
    use super::{allocate_zeroed_backend_buffer, parse_capacity_bytes};

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
}
