//! Unified emulated-device adapter for VirtIO MMIO block devices.

use alloc::{boxed::Box, string::String, sync::Arc};

use axaddrspace::GuestMemoryAccessor;
use axdevice_base::{
    BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceError, InterruptTriggerMode,
    IrqLine, Resource,
};
use axvm_types::GuestPhysAddr;

use crate::{BlockBackend, BlockDeviceEvent, VirtioError, VirtioMmioBlockDevice};

/// A VirtIO block model registered in the unified emulated-device runtime.
pub struct ManagedVirtioBlockDevice<B, T>
where
    B: BlockBackend,
    T: GuestMemoryAccessor + Clone,
{
    name: String,
    model: Arc<VirtioMmioBlockDevice<B, T>>,
    irq: IrqLine,
    resources: Box<[Resource]>,
}

impl<B, T> ManagedVirtioBlockDevice<B, T>
where
    B: BlockBackend,
    T: GuestMemoryAccessor + Clone,
{
    /// Creates a managed device with exclusive MMIO and edge-triggered IRQ resources.
    pub fn new(
        name: String,
        model: Arc<VirtioMmioBlockDevice<B, T>>,
        irq: IrqLine,
        mmio_base: u64,
        mmio_size: u64,
        irq_line: u32,
    ) -> Self {
        let resources = alloc::vec![
            Resource::MmioRange {
                base: mmio_base,
                size: mmio_size,
            },
            Resource::IrqLine {
                line: irq_line,
                trigger: InterruptTriggerMode::EdgeTriggered,
            },
        ]
        .into_boxed_slice();
        Self {
            name,
            model,
            irq,
            resources,
        }
    }

    /// Returns the underlying VirtIO block transport model.
    pub fn model(&self) -> &Arc<VirtioMmioBlockDevice<B, T>> {
        &self.model
    }
}

impl<B, T> Device for ManagedVirtioBlockDevice<B, T>
where
    B: BlockBackend,
    T: GuestMemoryAccessor + Clone + Send + Sync,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::InvalidInput {
                operation: "access virtio-block device",
                detail: String::from("virtio-block only supports MMIO accesses"),
            });
        }

        let address = GuestPhysAddr::from(access.addr as usize);
        if access.is_read {
            let value = self
                .model
                .mmio_read(address, access.width)
                .map_err(map_virtio_error)?;
            Ok(BusResponse::Read {
                value: value as u64,
            })
        } else {
            let interrupt_before = self.model.interrupt_status();
            let reassert_interrupt = self
                .model
                .mmio_write(address, access.width, access.data as usize)
                .map_err(map_virtio_error)?;
            let interrupt_after = self.model.interrupt_status();
            if reassert_interrupt == BlockDeviceEvent::InterruptPending
                || interrupt_after & !interrupt_before != 0
            {
                self.irq.pulse().map_err(|error| DeviceError::Backend {
                    operation: "pulse virtio-block interrupt",
                    detail: alloc::format!("{error}"),
                })?;
            }
            Ok(BusResponse::Write)
        }
    }
}

fn map_virtio_error(error: VirtioError) -> DeviceError {
    DeviceError::InvalidInput {
        operation: "access virtio-block MMIO transport",
        detail: alloc::format!("{error:?}"),
    }
}
