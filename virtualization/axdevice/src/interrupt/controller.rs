//! Interrupt-controller registration capabilities.

use alloc::sync::Arc;

use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptTriggerMode, IrqLine, IrqResult,
    MsiDeviceId, MsiEndpoint, MsiEventId, WiredIrqInput,
};

use super::{VcpuInterruptController, VcpuInterruptDeactivation, WiredIrqRequest};

/// Declares whether a controller is selected by legacy/default requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerRole {
    /// The controller is the topology's single default controller.
    Default,
    /// The controller must be selected explicitly or through a cascade.
    Secondary,
}

/// Supplies controller-owned wired inputs to the topology.
pub trait WiredInterruptInputs: Send + Sync {
    /// Opens a controller input using the requested trigger mode.
    ///
    /// Implementations must return the same shared [`WiredIrqInput`] state for
    /// repeated requests targeting the same hardware input.
    fn input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput>;
}

/// Supplies message-signaled endpoints to the topology.
pub trait MessageInterruptInputs: Send + Sync {
    /// Connects one MSI-producing device event.
    fn connect(&self, device: MsiDeviceId, event: MsiEventId) -> IrqResult<MsiEndpoint>;
}

/// Receives the cascade output line of a child controller.
pub trait InterruptControllerOutput: Send + Sync {
    /// Connects the controller's aggregate output to its parent input.
    fn connect_output(&self, line: IrqLine) -> IrqResult;

    /// Disconnects a previously connected aggregate output.
    fn disconnect_output(&self) -> IrqResult;
}

/// Describes one child-to-parent controller connection.
#[derive(Clone)]
pub struct ControllerCascade {
    parent: WiredIrqRequest,
    output: Arc<dyn InterruptControllerOutput>,
}

impl ControllerCascade {
    /// Creates a cascade declaration.
    pub fn new(parent: WiredIrqRequest, output: Arc<dyn InterruptControllerOutput>) -> Self {
        Self { parent, output }
    }

    /// Returns the requested parent input.
    pub const fn parent(&self) -> WiredIrqRequest {
        self.parent
    }

    pub(crate) fn connect_output(&self, line: IrqLine) -> IrqResult {
        self.output.connect_output(line)
    }

    pub(crate) fn disconnect_output(&self) -> IrqResult {
        self.output.disconnect_output()
    }
}

impl core::fmt::Debug for ControllerCascade {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ControllerCascade")
            .field("parent", &self.parent)
            .finish_non_exhaustive()
    }
}

/// Capabilities contributed by one interrupt controller.
#[derive(Clone)]
pub struct ControllerRegistration {
    id: InterruptControllerId,
    role: ControllerRole,
    wired_inputs: Option<Arc<dyn WiredInterruptInputs>>,
    message_inputs: Option<Arc<dyn MessageInterruptInputs>>,
    vcpu_controller: Option<Arc<dyn VcpuInterruptController>>,
    vcpu_deactivation: Option<Arc<dyn VcpuInterruptDeactivation>>,
    cascade: Option<ControllerCascade>,
}

impl ControllerRegistration {
    /// Starts a registration for `id` with no optional capabilities.
    pub const fn new(id: InterruptControllerId, role: ControllerRole) -> Self {
        Self {
            id,
            role,
            wired_inputs: None,
            message_inputs: None,
            vcpu_controller: None,
            vcpu_deactivation: None,
            cascade: None,
        }
    }

    /// Adds wired controller inputs.
    pub fn with_wired_inputs(mut self, inputs: Arc<dyn WiredInterruptInputs>) -> Self {
        self.wired_inputs = Some(inputs);
        self
    }

    /// Adds message-signaled controller inputs.
    pub fn with_message_inputs(mut self, inputs: Arc<dyn MessageInterruptInputs>) -> Self {
        self.message_inputs = Some(inputs);
        self
    }

    /// Adds vCPU attachment support.
    pub fn with_vcpu_controller(mut self, controller: Arc<dyn VcpuInterruptController>) -> Self {
        self.vcpu_controller = Some(controller);
        self
    }

    /// Adds support for architecture-trapped interrupt deactivation.
    pub fn with_vcpu_deactivation(
        mut self,
        deactivation: Arc<dyn VcpuInterruptDeactivation>,
    ) -> Self {
        self.vcpu_deactivation = Some(deactivation);
        self
    }

    /// Declares a cascade connection to a parent controller.
    pub fn with_cascade(mut self, cascade: ControllerCascade) -> Self {
        self.cascade = Some(cascade);
        self
    }

    /// Returns the VM-local controller identifier.
    pub const fn id(&self) -> InterruptControllerId {
        self.id
    }

    /// Returns the controller's selection role.
    pub const fn role(&self) -> ControllerRole {
        self.role
    }

    pub(crate) fn wired_inputs(&self) -> Option<&Arc<dyn WiredInterruptInputs>> {
        self.wired_inputs.as_ref()
    }

    pub(crate) fn message_inputs(&self) -> Option<&Arc<dyn MessageInterruptInputs>> {
        self.message_inputs.as_ref()
    }

    pub(crate) fn vcpu_controller(&self) -> Option<&Arc<dyn VcpuInterruptController>> {
        self.vcpu_controller.as_ref()
    }

    pub(crate) fn vcpu_deactivation(&self) -> Option<&Arc<dyn VcpuInterruptDeactivation>> {
        self.vcpu_deactivation.as_ref()
    }

    pub(crate) const fn cascade(&self) -> Option<&ControllerCascade> {
        self.cascade.as_ref()
    }
}

impl core::fmt::Debug for ControllerRegistration {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ControllerRegistration")
            .field("id", &self.id)
            .field("role", &self.role)
            .field("has_wired_inputs", &self.wired_inputs.is_some())
            .field("has_message_inputs", &self.message_inputs.is_some())
            .field("has_vcpu_controller", &self.vcpu_controller.is_some())
            .field("has_vcpu_deactivation", &self.vcpu_deactivation.is_some())
            .field("cascade", &self.cascade)
            .finish()
    }
}
