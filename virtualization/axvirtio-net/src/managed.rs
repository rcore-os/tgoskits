//! Unified emulated-device adapter for VirtIO MMIO network devices.

use alloc::{boxed::Box, string::String, sync::Arc};

use axaddrspace::GuestMemoryAccessor;
use axdevice_base::{
    BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceError, InterruptTriggerMode,
    IrqLine, Resource,
};
use axvm_types::GuestPhysAddr;

use crate::{DeviceEvent, NetworkBackend, VirtioError, VirtioMmioNetDevice};

/// A VirtIO-net model registered in the unified emulated-device runtime.
pub struct ManagedVirtioNetDevice<B, T>
where
    B: NetworkBackend,
    T: GuestMemoryAccessor + Clone,
{
    name: String,
    model: Arc<VirtioMmioNetDevice<B, T>>,
    irq: IrqLine,
    resources: Box<[Resource]>,
}

impl<B, T> ManagedVirtioNetDevice<B, T>
where
    B: NetworkBackend,
    T: GuestMemoryAccessor + Clone,
{
    /// Creates a managed device with exclusive MMIO and edge-triggered IRQ resources.
    pub fn new(
        name: String,
        model: Arc<VirtioMmioNetDevice<B, T>>,
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

    /// Returns the transport model shared with the receive path.
    pub fn model(&self) -> &Arc<VirtioMmioNetDevice<B, T>> {
        &self.model
    }

    /// Returns the interrupt line used by host-driven receive completion.
    pub fn irq(&self) -> &IrqLine {
        &self.irq
    }
}

impl<B, T> Device for ManagedVirtioNetDevice<B, T>
where
    B: NetworkBackend,
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
                operation: "access virtio-net device",
                detail: String::from("virtio-net only supports MMIO accesses"),
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
            let event = self
                .model
                .mmio_write(address, access.width, access.data as usize)
                .map_err(map_virtio_error)?;
            if event == DeviceEvent::InterruptPending {
                self.irq.pulse().map_err(|error| DeviceError::Backend {
                    operation: "pulse virtio-net interrupt",
                    detail: alloc::format!("{error}"),
                })?;
            }
            Ok(BusResponse::Write)
        }
    }
}

fn map_virtio_error(error: VirtioError) -> DeviceError {
    DeviceError::InvalidInput {
        operation: "access virtio-net MMIO transport",
        detail: alloc::format!("{error:?}"),
    }
}
