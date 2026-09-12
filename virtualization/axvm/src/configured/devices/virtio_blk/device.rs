//! Configured VirtIO block device with MMIO and modern PCI transports.
//!
//! MMIO devices may use memory or file-backed storage; the current PCI
//! configuration accepts the synchronous ramdisk backend.

use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "fs")]
use std::string::String;
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
    InterruptTrigger, IrqLine, IrqResult, Resource,
};
use axvirtio_blk::{
    BlockBackend, BlockDeviceEvent, VirtioBlockConfig, VirtioBlockPciAdapter, VirtioMmioBlockDevice,
};
use axvirtio_common::{
    GuestMemory, NoGuestMemoryAccessor, VirtioError, VirtioResult, map_virtio_error,
};
use axvm_types::GuestPhysAddr;
use axvmconfig::VirtualDeviceRequest;

use super::{
    super::virtio_pci::{VirtioPciFunction, virtio_capabilities},
    options::{BackendConfig, FilesystemFormat, parse_backend},
};
#[cfg(feature = "fs")]
use super::{
    file::FileBackend,
    image::{ImageReader, inspect_file_image},
};
use crate::{ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext};

const MMIO_SLOT: &str = "mmio";
const IRQ_SLOT: &str = "irq";
const MMIO_SIZE: u64 = 0x200;
const SECTOR_SIZE: usize = 512;
const DEFAULT_CAPACITY_BYTES: u64 = 2 * 1024 * 1024;
const PCI_BAR_SIZE: u64 = 0x1000;
const PCI_INTX_SLOT: &str = "virtio-intx";
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
    validate_known_options(request)?;
    let transport = parse_transport(request)?;
    let capacity_bytes =
        parse_capacity(request).map_err(|detail| invalid_options(request, detail))?;
    let backend_config =
        parse_backend(request).map_err(|detail| invalid_options(request, detail))?;
    let read_only = parse_read_only(request)?;
    let host = match transport {
        VirtioBlkTransport::Mmio => None,
        VirtioBlkTransport::Pci => {
            Some(context.default_pci_host_key().cloned().ok_or_else(|| {
                ConfiguredDeviceError::Instantiation {
                    device: request.id.clone(),
                    model: request.model.clone(),
                    detail: "virtio-blk PCI transport requires a configured PCI host".into(),
                }
            })?)
        }
    };
    if matches!(&transport, VirtioBlkTransport::Pci)
        && !matches!(&backend_config, BackendConfig::RamDisk { .. })
    {
        return Err(invalid_options(
            request,
            "PCI transport requires `backend = \"ramdisk\"`",
        ));
    }
    let vm_id = match &backend_config {
        BackendConfig::RamDisk { .. } => None,
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
    let backend =
        VirtioBlkBackend::open(&backend_config, capacity_bytes, vm_id).map_err(|error| {
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
        transport: match (transport, host) {
            (VirtioBlkTransport::Mmio, None) => VirtioBlkTransportConfig::Mmio,
            (VirtioBlkTransport::Pci, Some(host)) => VirtioBlkTransportConfig::Pci { host },
            _ => unreachable!("transport and PCI host are validated together"),
        },
        read_only,
    });
    let mut node = DeviceNodeSpec::virtual_device(id, model);
    if let Some(controller_node) = context.default_wired_controller_node() {
        node = node.with_dependency(controller_node.clone());
    }
    Ok(node)
}

fn validate_known_options(request: &VirtualDeviceRequest) -> Result<(), ConfiguredDeviceError> {
    for key in request.options.keys() {
        if !matches!(
            key.as_str(),
            "transport"
                | "backend"
                | "capacity"
                | "capacity_sectors"
                | "path"
                | "image_path"
                | "read_only"
                | "filesystem"
        ) {
            return Err(invalid_options(
                request,
                &format!("unknown virtio-blk option `{key}`"),
            ));
        }
    }
    Ok(())
}

fn parse_transport(
    request: &VirtualDeviceRequest,
) -> Result<VirtioBlkTransport, ConfiguredDeviceError> {
    match request.options.get("transport") {
        None => Ok(VirtioBlkTransport::Mmio),
        Some(value) => match value.as_str() {
            Some("mmio") => Ok(VirtioBlkTransport::Mmio),
            Some("pci") => Ok(VirtioBlkTransport::Pci),
            _ => Err(invalid_options(
                request,
                "`transport` must be `mmio` or `pci`",
            )),
        },
    }
}

fn parse_read_only(request: &VirtualDeviceRequest) -> Result<bool, ConfiguredDeviceError> {
    request
        .options
        .get("read_only")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| invalid_options(request, "`read_only` must be a boolean"))
        })
        .transpose()
        .map(|value| value.unwrap_or(false))
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
    transport: VirtioBlkTransportConfig,
    read_only: bool,
}

enum VirtioBlkTransportConfig {
    Mmio,
    Pci { host: PciHostKey },
}

enum VirtioBlkTransport {
    Mmio,
    Pci,
}

impl DeviceModel for VirtioBlkModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        let irq = ResourceSlot::new(PCI_INTX_SLOT)?;
        match &self.transport {
            VirtioBlkTransportConfig::Mmio => DeviceRequirements::new()
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
                ),
            VirtioBlkTransportConfig::Pci { host } => {
                let capabilities =
                    virtio_capabilities(&axvirtio_common::pci::VirtioPciCapabilitySet::new(16))
                        .map_err(DeviceManagerError::Pci)?;
                let mut requirement = PciFunctionRequirement::new(
                    host.clone(),
                    PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0x01, 0x80, 0x00))
                        .with_revision(1)
                        .with_subsystem_ids(0x1af4, 0x1042),
                )
                .with_bar(PciMemoryBar::new(PciBarIndex::new(0)?, PCI_BAR_SIZE)?)?
                .with_intx(PciIntxRequirement::new(PciIntxPin::A, irq))?;
                for capability in capabilities {
                    requirement = requirement.with_capability(capability);
                }
                DeviceRequirements::new().with_pci_function(requirement)
            }
        }
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        if matches!(&self.transport, VirtioBlkTransportConfig::Pci { .. }) {
            return DeviceFirmwareSpec::None;
        }
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
            read_only: self.read_only,
            flush_supported: !self.read_only,
            ..Default::default()
        };
        match &self.transport {
            VirtioBlkTransportConfig::Mmio => {
                let (base, size) = context.mmio(MMIO_SLOT)?;
                let irq = context.irq(IRQ_SLOT)?;
                let resolved_irq = irq.input().value();
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
            VirtioBlkTransportConfig::Pci { .. } => {
                if backend.requires_deferred_processing() {
                    return Err(invalid_device_config(
                        "construct virtio-blk PCI device",
                        "PCI transport requires a synchronous backend",
                    ));
                }
                let irq = context.irq(PCI_INTX_SLOT)?;
                let grant = DmaGrant::new();
                let function = Arc::new(
                    VirtioPciFunction::try_new(
                        VirtioBlockPciAdapter::new(backend, config),
                        grant.clone(),
                        irq,
                    )
                    .map_err(DeviceManagerError::Device)?,
                );
                let mut bundle = DeviceBundle::new();
                let device_index = bundle.add_pci_function(function)?;
                bundle.grant_guest_memory_to_device(device_index, grant);
                Ok(bundle)
            }
        }
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
        config: &BackendConfig,
        capacity_bytes: Option<u64>,
        vm_id: Option<usize>,
    ) -> DeviceManagerResult<Self> {
        match config {
            BackendConfig::RamDisk { image_path } => {
                if let Some(path) = image_path {
                    open_ramdisk_image(path, capacity_bytes)
                } else {
                    let capacity = capacity_bytes.unwrap_or(DEFAULT_CAPACITY_BYTES);
                    Ok(Self::RamDisk(RamDiskBackend::new(capacity)?))
                }
            }
            BackendConfig::File { path, filesystem } => {
                let vm_id = vm_id.ok_or_else(|| {
                    invalid_device_config(
                        "open virtio-blk backing file",
                        "file backend requires a VM identity",
                    )
                })?;
                open_file_backend(path, capacity_bytes, *filesystem, vm_id)
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

#[cfg(any(feature = "fs", test))]
fn validate_ramdisk_image_size(
    image_size: u64,
    configured_capacity: Option<u64>,
) -> DeviceManagerResult<()> {
    if image_size == 0 {
        return Err(invalid_device_config(
            "validate virtio-blk ramdisk image",
            "backing image must not be empty",
        ));
    }
    if !image_size.is_multiple_of(SECTOR_SIZE as u64) {
        return Err(invalid_device_config(
            "validate virtio-blk ramdisk image",
            "backing image length must be a multiple of 512 bytes",
        ));
    }
    if let Some(configured_capacity) = configured_capacity
        && configured_capacity != image_size
    {
        return Err(invalid_device_config(
            "validate virtio-blk ramdisk image",
            &format!(
                "configured capacity {configured_capacity} does not match backing image length \
                 {image_size}"
            ),
        ));
    }
    Ok(())
}

#[cfg(feature = "fs")]
fn open_ramdisk_image(
    path: &str,
    configured_capacity: Option<u64>,
) -> DeviceManagerResult<VirtioBlkBackend> {
    let mut options = ax_api::fs::AxOpenOptions::new();
    options.read(true);
    let file = ax_api::fs::ax_open_file(path, &options).map_err(|error| {
        invalid_device_config(
            "open virtio-blk ramdisk image",
            &format!("failed to open `{path}`: {error}"),
        )
    })?;
    let image_size = ax_api::fs::ax_file_attr(&file)
        .map_err(|error| {
            invalid_device_config(
                "inspect virtio-blk ramdisk image",
                &format!("failed to inspect `{path}`: {error}"),
            )
        })?
        .size;
    validate_ramdisk_image_size(image_size, configured_capacity)?;
    let mut image = allocate_zeroed_backend_buffer(image_size, "load virtio-blk ramdisk image")?;
    let read = ax_api::fs::ax_read_file_at(&file, 0, &mut image).map_err(|error| {
        invalid_device_config(
            "load virtio-blk ramdisk image",
            &format!("failed to read `{path}`: {error}"),
        )
    })?;
    if read != image.len() {
        return Err(invalid_device_config(
            "load virtio-blk ramdisk image",
            &format!(
                "backing image `{path}` returned {read} bytes, expected {}",
                image.len()
            ),
        ));
    }
    Ok(VirtioBlkBackend::RamDisk(RamDiskBackend::from_bytes(image)))
}

#[cfg(not(feature = "fs"))]
fn open_ramdisk_image(
    path: &str,
    _configured_capacity: Option<u64>,
) -> DeviceManagerResult<VirtioBlkBackend> {
    Err(invalid_device_config(
        "open virtio-blk ramdisk image",
        &format!("ramdisk image `{path}` requires the AxVM `fs` feature"),
    ))
}

#[cfg(feature = "fs")]
fn open_file_backend(
    path: &str,
    configured_capacity: Option<u64>,
    filesystem: FilesystemFormat,
    vm_id: usize,
) -> DeviceManagerResult<VirtioBlkBackend> {
    let mut options = ax_api::fs::AxOpenOptions::new();
    options.read(true);
    options.write(true);
    let file = ax_api::fs::ax_open_file(path, &options).map_err(|error| {
        invalid_device_config(
            "open virtio-blk backing file",
            &format!("failed to open `{path}`: {error}"),
        )
    })?;
    let mut reader = AxFileReader { file: &file };
    let capacity =
        inspect_file_image(&mut reader, configured_capacity, filesystem).map_err(|error| {
            invalid_device_config(
                "prepare virtio-blk backing file",
                &format!("failed to prepare `{path}`: {error}"),
            )
        })?;
    FileBackend::new(file, capacity / SECTOR_SIZE as u64, vm_id).map(VirtioBlkBackend::File)
}

#[cfg(feature = "fs")]
struct AxFileReader<'a> {
    file: &'a ax_api::fs::AxFileHandle,
}

#[cfg(feature = "fs")]
impl ImageReader for AxFileReader<'_> {
    fn len(&self) -> Result<u64, String> {
        ax_api::fs::ax_file_attr(self.file)
            .map(|attribute| attribute.size)
            .map_err(|error| format!("{error}"))
    }

    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, String> {
        ax_api::fs::ax_read_file_at(self.file, offset, bytes).map_err(|error| format!("{error}"))
    }
}

#[cfg(not(feature = "fs"))]
fn open_file_backend(
    path: &str,
    _configured_capacity: Option<u64>,
    _filesystem: FilesystemFormat,
    _vm_id: usize,
) -> DeviceManagerResult<VirtioBlkBackend> {
    Err(invalid_device_config(
        "open virtio-blk backing file",
        &format!("file backend `{path}` requires the AxVM `fs` feature"),
    ))
}

impl BlockBackend for VirtioBlkBackend {
    fn pending_request_ready(&self) -> bool {
        match self {
            Self::RamDisk(backend) => backend.pending_request_ready(),
            #[cfg(feature = "fs")]
            Self::File(backend) => backend.pending_request_ready(),
        }
    }

    fn cancel_pending_request(&self) {
        match self {
            Self::RamDisk(backend) => backend.cancel_pending_request(),
            #[cfg(feature = "fs")]
            Self::File(backend) => backend.cancel_pending_request(),
        }
    }

    fn reset(&self) {
        match self {
            Self::RamDisk(backend) => backend.reset(),
            #[cfg(feature = "fs")]
            Self::File(backend) => backend.reset(),
        }
    }

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
        Ok(Self::from_bytes(allocate_zeroed_backend_buffer(
            capacity_bytes,
            "allocate virtio-blk ramdisk",
        )?))
    }

    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            capacity_sectors: bytes.len() as u64 / SECTOR_SIZE as u64,
            bytes: Mutex::new(bytes),
        }
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
            .map_err(|error| map_virtio_error(error, "access virtio-blk MMIO transport"))
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
            .map_err(|error| map_virtio_error(error, "access virtio-blk MMIO transport"))?;
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

#[cfg(test)]
mod tests {
    use axdevice_base::InterruptControllerId;
    use toml::map::Map;

    use super::{super::register, *};
    use crate::ConfiguredDeviceCatalog;

    fn request(options: &[(&str, toml::Value)]) -> VirtualDeviceRequest {
        VirtualDeviceRequest {
            id: "disk0".into(),
            model: "virtio-blk".into(),
            options: options
                .iter()
                .map(|(key, value)| ((*key).into(), value.clone()))
                .collect::<Map<_, _>>(),
        }
    }

    fn context(with_pci_host: bool) -> DeviceInstantiationContext {
        let controller = DeviceNodeId::new("controller").unwrap();
        let context = DeviceInstantiationContext::new().with_default_wired_controller(
            controller,
            axdevice_base::InterruptControllerId::new(0),
        );
        if with_pci_host {
            context.with_default_pci_host_key(PciHostKey::new("x86-q35").unwrap())
        } else {
            context
        }
    }

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
        let mut request = virtual_device_request();
        request
            .options
            .insert("filesystem".into(), toml::Value::String("ext4".into()));
        let error = match registered_catalog().instantiate_node(&request, &context_without_vm_id())
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

    #[test]
    fn ramdisk_image_size_must_be_nonempty_aligned_and_match_capacity() {
        assert!(validate_ramdisk_image_size(0, None).is_err());
        assert!(validate_ramdisk_image_size(513, None).is_err());
        assert!(validate_ramdisk_image_size(1024, Some(512)).is_err());
        assert!(validate_ramdisk_image_size(1024, Some(1536)).is_err());
        assert!(validate_ramdisk_image_size(1024, Some(1024)).is_ok());
    }

    #[test]
    fn ramdisk_initialized_from_bytes_serves_the_image() {
        let mut bytes = vec![0; 1024];
        bytes[512..].fill(0xa5);
        let backend = RamDiskBackend::from_bytes(bytes);
        let mut sector = [0; 512];

        assert_eq!(backend.read(1, &mut sector), Ok(512));
        assert!(sector.iter().all(|byte| *byte == 0xa5));
        assert_eq!(backend.capacity_sectors, 2);
    }

    #[test]
    fn unknown_options_are_rejected_before_backend_creation() {
        let request = request(&[
            ("backend", toml::Value::String("ramdisk".into())),
            ("unknown", toml::Value::Boolean(true)),
        ]);
        assert!(matches!(
            create_device_node(
                DeviceNodeId::new("disk0").unwrap(),
                &request,
                &context(false)
            ),
            Err(ConfiguredDeviceError::InvalidOptions { .. })
        ));
    }

    #[test]
    fn pci_transport_requires_explicit_ramdisk_backend() {
        let request = request(&[("transport", toml::Value::String("pci".into()))]);
        let result = create_device_node(
            DeviceNodeId::new("disk0").unwrap(),
            &request,
            &context(true),
        );
        assert!(matches!(
            result,
            Err(ConfiguredDeviceError::InvalidOptions { .. })
        ));
    }

    #[test]
    fn pci_transport_declares_only_pci_resources() {
        let request = request(&[
            ("transport", toml::Value::String("pci".into())),
            ("backend", toml::Value::String("ramdisk".into())),
            ("capacity", toml::Value::String("1MiB".into())),
        ]);
        let node = create_device_node(
            DeviceNodeId::new("disk0").unwrap(),
            &request,
            &context(true),
        )
        .unwrap();
        let mut builder = DeviceGraphBuilder::new();
        builder
            .add(DeviceNodeSpec::firmware_only(
                DeviceNodeId::new("controller").unwrap(),
            ))
            .unwrap();
        builder.add(node).unwrap();
        let requests = builder.requests().unwrap();
        let requirements = requests
            .iter()
            .find(|request| request.id() == "disk0")
            .unwrap()
            .requirements();
        assert!(requirements.entries().is_empty());
        let pci = requirements.pci_function().unwrap();
        assert_eq!(pci.host(), &PciHostKey::new("x86-q35").unwrap());
    }

    #[test]
    fn default_transport_remains_mmio() {
        let request = request(&[("backend", toml::Value::String("ramdisk".into()))]);
        let node = create_device_node(
            DeviceNodeId::new("disk0").unwrap(),
            &request,
            &context(false),
        )
        .unwrap();
        let mut builder = DeviceGraphBuilder::new();
        builder
            .add(DeviceNodeSpec::firmware_only(
                DeviceNodeId::new("controller").unwrap(),
            ))
            .unwrap();
        builder.add(node).unwrap();
        let requests = builder.requests().unwrap();
        let requirements = requests
            .iter()
            .find(|request| request.id() == "disk0")
            .unwrap()
            .requirements();
        assert!(requirements.pci_function().is_none());
        assert_eq!(requirements.entries().len(), 2);
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
