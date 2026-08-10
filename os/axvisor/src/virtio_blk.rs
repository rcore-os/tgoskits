//! Configured VirtIO MMIO block device backed by an in-memory disk.

use alloc::{format, sync::Arc, vec, vec::Vec};
use std::sync::Mutex;

use axdevice::*;
use axdevice_base::{
    BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceError, DmaGrant, InterruptSharing,
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
const DEFAULT_CAPACITY_SECTORS: usize = 4096;

/// Catalog entry for `[[devices.virtual]] model = "virtio-blk"`.
pub const REGISTRATION: ConfiguredModelRegistration = ConfiguredModelRegistration {
    model: "virtio-blk",
    create: create_device_node,
};

fn create_device_node(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    let capacity_sectors = request
        .options
        .get("capacity_sectors")
        .map(|value| {
            value
                .as_integer()
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid_options(request, "`capacity_sectors` must be positive"))
        })
        .transpose()?
        .unwrap_or(DEFAULT_CAPACITY_SECTORS);
    let controller =
        context
            .default_wired_controller()
            .ok_or_else(|| ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: "virtio-blk requires a wired interrupt controller".into(),
            })?;
    let model: Arc<dyn DeviceModel> = Arc::new(VirtioBlkModel {
        capacity_sectors,
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

struct VirtioBlkModel {
    capacity_sectors: usize,
    controller: axdevice_base::InterruptControllerId,
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
        let mut config = VirtioBlockConfig::default();
        config.capacity = self.capacity_sectors as u64;
        let model = Arc::new(
            VirtioMmioBlockDevice::new(
                GuestPhysAddr::from(base as usize),
                size as usize,
                RamDiskBackend::new(self.capacity_sectors)?,
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
        bundle.add_guest_memory_device_with_grant(device, grant);
        Ok(bundle)
    }
}

struct RamDiskBackend {
    bytes: Mutex<Vec<u8>>,
}

impl RamDiskBackend {
    fn new(capacity_sectors: usize) -> DeviceManagerResult<Self> {
        let byte_len = capacity_sectors.checked_mul(SECTOR_SIZE).ok_or_else(|| {
            DeviceManagerError::InvalidConfig {
                operation: "allocate virtio-blk ramdisk",
                detail: "capacity overflows usize".into(),
            }
        })?;
        Ok(Self {
            bytes: Mutex::new(vec![0; byte_len]),
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
    access: &'a mut dyn DeviceAccess,
    grant: &'a DmaGrant,
}

impl GuestMemory for ScopedDeviceMemory<'_> {
    fn read(&mut self, guest_addr: GuestPhysAddr, data: &mut [u8]) -> VirtioResult<()> {
        self.access
            .read_guest_memory(self.grant, guest_addr, data)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    fn write(&mut self, guest_addr: GuestPhysAddr, data: &[u8]) -> VirtioResult<()> {
        self.access
            .write_guest_memory(self.grant, guest_addr, data)
            .map_err(|_| VirtioError::InvalidAddress)
    }
}

struct VirtioBlkRuntimeDevice {
    model: Arc<VirtioMmioBlockDevice<RamDiskBackend, NoGuestMemoryAccessor>>,
    irq: IrqLine,
    grant: DmaGrant,
    resources: alloc::boxed::Box<[Resource]>,
}

impl Device for VirtioBlkRuntimeDevice {
    fn name(&self) -> &str {
        "virtio-blk"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let address = GuestPhysAddr::from(access.addr as usize);
        if access.is_read {
            return self
                .model
                .mmio_read(address, access.width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
                .map_err(map_virtio_error);
        }
        let mut memory = ScopedDeviceMemory {
            access: context,
            grant: &self.grant,
        };
        let event = self
            .model
            .mmio_write_with_memory(address, access.width, access.data as usize, &mut memory)
            .map_err(map_virtio_error)?;
        if event == BlockDeviceEvent::InterruptPending {
            self.irq.pulse().map_err(|error| DeviceError::Backend {
                operation: "pulse virtio-blk interrupt",
                detail: format!("{error}"),
            })?;
        }
        Ok(BusResponse::Write)
    }
}

fn map_virtio_error(error: VirtioError) -> DeviceError {
    DeviceError::InvalidInput {
        operation: "access virtio-blk MMIO transport",
        detail: format!("{error:?}"),
    }
}
