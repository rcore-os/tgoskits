//! Message-signaled interrupt endpoints.

use alloc::sync::Arc;

use super::{
    InterruptControllerId, InterruptEndpoint, IrqResult, ItsId, LpiId, MsiDeviceId, MsiEventId,
};

/// One message delivered to an MSI-capable interrupt controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsiMessage {
    its: ItsId,
    device: MsiDeviceId,
    event: MsiEventId,
    lpi: LpiId,
}

impl MsiMessage {
    /// Creates a planned MSI message.
    pub const fn new(its: ItsId, device: MsiDeviceId, event: MsiEventId, lpi: LpiId) -> Self {
        Self {
            its,
            device,
            event,
            lpi,
        }
    }

    /// Returns the receiving ITS.
    pub const fn its(self) -> ItsId {
        self.its
    }

    /// Returns the MSI-producing device.
    pub const fn device(self) -> MsiDeviceId {
        self.device
    }

    /// Returns the device-local event.
    pub const fn event(self) -> MsiEventId {
        self.event
    }

    /// Returns the planner-reserved LPI.
    pub const fn lpi(self) -> LpiId {
        self.lpi
    }
}

/// Supplies planner-authorized MSI endpoints to virtual devices.
pub trait MessageInterruptController: Send + Sync {
    /// Returns the controller identifier used by the VM device registry.
    fn id(&self) -> InterruptControllerId;

    /// Opens one planned message endpoint.
    fn msi_endpoint(
        &self,
        its: ItsId,
        device: MsiDeviceId,
        event: MsiEventId,
        lpi: LpiId,
    ) -> IrqResult<MsiEndpoint>;
}

/// Receives message-signaled interrupts for one controller.
pub trait MessageInterruptSink: Send + Sync {
    /// Delivers one validated message to the controller.
    fn signal(&self, message: MsiMessage) -> IrqResult;
}

/// A device-owned connection to a message interrupt controller.
#[derive(Clone)]
pub struct MsiEndpoint {
    controller: InterruptControllerId,
    message: MsiMessage,
    sink: Arc<dyn MessageInterruptSink>,
}

impl MsiEndpoint {
    /// Creates an endpoint for a controller implementation.
    pub fn new(
        controller: InterruptControllerId,
        message: MsiMessage,
        sink: Arc<dyn MessageInterruptSink>,
    ) -> Self {
        Self {
            controller,
            message,
            sink,
        }
    }

    /// Delivers the endpoint's message.
    pub fn signal(&self) -> IrqResult {
        self.sink.signal(self.message)
    }

    /// Returns the receiving controller.
    pub const fn controller(&self) -> InterruptControllerId {
        self.controller
    }

    /// Returns the bound message.
    pub const fn message(&self) -> MsiMessage {
        self.message
    }

    /// Returns this endpoint in diagnostic form.
    pub const fn diagnostic_endpoint(&self) -> InterruptEndpoint {
        InterruptEndpoint::Message {
            controller: self.controller,
            its: self.message.its,
            device: self.message.device,
            event: self.message.event,
            lpi: self.message.lpi,
        }
    }
}

impl core::fmt::Debug for MsiEndpoint {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("MsiEndpoint")
            .field("controller", &self.controller)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}
