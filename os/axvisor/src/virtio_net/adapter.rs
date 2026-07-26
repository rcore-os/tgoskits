//! AxVisor device adapter that connects [`VirtioMmioNetDevice`] to the AxVM
//! MMIO dispatch router.
//!
//! The adapter owns the device model, its edge-triggered [`IrqLine`] and the
//! stable resource list the router uses for address dispatch. It translates
//! [`BusAccess`] MMIO exits into device `mmio_read`/`mmio_write` calls and, when
//! a write reports [`DeviceEvent::InterruptPending`], pulses the IRQ so the
//! backend interrupt reaches the target vCPU through the VM-local queued sink.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;

use axdevice_base::{BusAccess, BusKind, BusResponse, Device, DeviceError, IrqLine, Resource};
use axvirtio_net::{DeviceEvent, VirtioError, VirtioMmioNetDevice};
use axvm::{AxvmGuestMemoryAccessor, GuestPhysAddr};

use super::backend::AxvisorNetworkBackend;
use super::raw_uplink::PortAttachment;

/// AxVisor adapter wrapping one virtio-net MMIO device model.
pub struct VirtioNetDeviceAdapter {
    name: String,
    device: Arc<VirtioMmioNetDevice<AxvisorNetworkBackend, AxvmGuestMemoryAccessor>>,
    irq: IrqLine,
    backend: AxvisorNetworkBackend,
    /// RAII switch-port attachment for raw-uplink devices. Dropping the adapter
    /// detaches the port (deactivate -> unregister); `None` for the
    /// deterministic echo backend, which owns no switch port. The field is
    /// drop-only by design, so it is never read directly.
    _attachment: Option<PortAttachment>,
    resources: Box<[Resource]>,
}

impl VirtioNetDeviceAdapter {
    /// Creates a new adapter from its prepared components.
    pub(super) fn new(
        name: String,
        device: Arc<VirtioMmioNetDevice<AxvisorNetworkBackend, AxvmGuestMemoryAccessor>>,
        irq: IrqLine,
        backend: AxvisorNetworkBackend,
        attachment: Option<PortAttachment>,
        resources: Box<[Resource]>,
    ) -> Self {
        Self {
            name,
            device,
            irq,
            backend,
            _attachment: attachment,
            resources,
        }
    }

    /// Returns the device model handle (shared with the RX worker).
    pub fn device(
        &self,
    ) -> &Arc<VirtioMmioNetDevice<AxvisorNetworkBackend, AxvmGuestMemoryAccessor>> {
        &self.device
    }

    /// Returns the interrupt line used to signal RX/TX completions.
    pub fn irq(&self) -> &IrqLine {
        &self.irq
    }

    /// Returns the backend handle (shared with the RX worker).
    pub fn backend(&self) -> &AxvisorNetworkBackend {
        &self.backend
    }
}

impl Device for VirtioNetDeviceAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        // The virtio-net model only participates in the MMIO bus; reject port and
        // system-register accesses with a diagnostic instead of silently handling
        // them (plan section 3).
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::InvalidInput {
                operation: "virtio-net handle",
                detail: String::from("only MMIO bus accesses are supported"),
            });
        }

        let address = GuestPhysAddr::from(access.addr as usize);
        if access.is_read {
            let value = self
                .device
                .mmio_read(address, access.width)
                .map_err(map_virtio_error)?;
            Ok(BusResponse::Read {
                value: value as u64,
            })
        } else {
            let event = self
                .device
                .mmio_write(address, access.width, access.data as usize)
                .map_err(map_virtio_error)?;
            // The transport completes its own reset on status=0 writes; the
            // adapter only forwards the interrupt event and never re-resets.
            if event == DeviceEvent::InterruptPending {
                if let Err(error) = self.irq.pulse() {
                    warn!("virtio-net[{}] IRQ pulse failed: {error:?}", self.name);
                }
            }
            Ok(BusResponse::Write)
        }
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

fn map_virtio_error(error: VirtioError) -> DeviceError {
    // All transport/queue faults surface as invalid-input with the underlying
    // variant so the MMIO router logs a diagnosable failure rather than a bare
    // "internal error".
    DeviceError::InvalidInput {
        operation: "virtio-net mmio",
        detail: alloc::format!("{error:?}"),
    }
}
