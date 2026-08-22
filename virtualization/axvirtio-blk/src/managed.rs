//! Unified emulated-device adapter for VirtIO MMIO block devices.

use alloc::{boxed::Box, string::String, sync::Arc};

use axaddrspace::GuestMemoryAccessor;
use axdevice_base::{
    BusKind, Device, DeviceAccess, DeviceContext, DeviceError, InterruptTriggerMode, IrqLine,
    Resource,
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

    fn read(
        &self,
        access: &DeviceAccess,
        _context: &mut dyn DeviceContext,
    ) -> Result<u64, DeviceError> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::InvalidInput {
                operation: "access virtio-block device",
                detail: String::from("virtio-block only supports MMIO accesses"),
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
        _context: &mut dyn DeviceContext,
    ) -> Result<(), DeviceError> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::InvalidInput {
                operation: "access virtio-block device",
                detail: String::from("virtio-block only supports MMIO accesses"),
            });
        }
        let interrupt_before = self.model.interrupt_status();
        let reassert_interrupt = self
            .model
            .mmio_write(
                GuestPhysAddr::from(access.address() as usize),
                access.width(),
                value as usize,
            )
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
        Ok(())
    }
}

fn map_virtio_error(error: VirtioError) -> DeviceError {
    DeviceError::InvalidInput {
        operation: "access virtio-block MMIO transport",
        detail: alloc::format!("{error:?}"),
    }
}
