//! Typed device-to-controller connection requests.

use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTriggerMode, MsiDeviceId,
    MsiEventId,
};

/// Selects an interrupt controller in one VM topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerRef {
    /// Selects the topology's single default controller.
    Default,
    /// Selects a controller explicitly.
    Id(InterruptControllerId),
}

/// Requests a wired connection to a controller input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WiredIrqRequest {
    controller: ControllerRef,
    input: ControllerInputId,
    trigger: InterruptTriggerMode,
    sharing: InterruptSharing,
}

impl WiredIrqRequest {
    /// Creates a request for the default controller.
    pub const fn new(
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
        sharing: InterruptSharing,
    ) -> Self {
        Self {
            controller: ControllerRef::Default,
            input,
            trigger,
            sharing,
        }
    }

    /// Creates a request for an explicitly selected controller.
    pub const fn for_controller(
        controller: InterruptControllerId,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
        sharing: InterruptSharing,
    ) -> Self {
        Self {
            controller: ControllerRef::Id(controller),
            input,
            trigger,
            sharing,
        }
    }

    /// Returns the requested controller selector.
    pub const fn controller(self) -> ControllerRef {
        self.controller
    }

    /// Returns the controller-local input number.
    pub const fn input(self) -> ControllerInputId {
        self.input
    }

    /// Returns the requested trigger mode.
    pub const fn trigger(self) -> InterruptTriggerMode {
        self.trigger
    }

    /// Returns the planned sharing policy.
    pub const fn sharing(self) -> InterruptSharing {
        self.sharing
    }
}

/// Requests an MSI connection to a controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsiRequest {
    controller: ControllerRef,
    device: MsiDeviceId,
    event: MsiEventId,
}

impl MsiRequest {
    /// Creates an MSI request for the default controller.
    pub const fn new(device: MsiDeviceId, event: MsiEventId) -> Self {
        Self {
            controller: ControllerRef::Default,
            device,
            event,
        }
    }

    /// Creates an MSI request for an explicitly selected controller.
    pub const fn for_controller(
        controller: InterruptControllerId,
        device: MsiDeviceId,
        event: MsiEventId,
    ) -> Self {
        Self {
            controller: ControllerRef::Id(controller),
            device,
            event,
        }
    }

    /// Returns the requested controller selector.
    pub const fn controller(self) -> ControllerRef {
        self.controller
    }

    /// Returns the MSI-producing device identifier.
    pub const fn device(self) -> MsiDeviceId {
        self.device
    }

    /// Returns the device-local event identifier.
    pub const fn event(self) -> MsiEventId {
        self.event
    }
}
