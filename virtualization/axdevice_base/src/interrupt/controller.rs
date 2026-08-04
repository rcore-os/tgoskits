//! Architecture-independent virtual interrupt-controller capability.

use super::{ControllerInputId, InterruptControllerId, InterruptTrigger, IrqResult, WiredIrqInput};

/// Supplies controller-owned wired inputs to virtual devices.
///
/// vCPU context, register emulation, firmware description, host IRQ routing,
/// and EOI/deactivation remain owned by the architecture controller.
pub trait VirtualInterruptController: Send + Sync {
    /// Returns the controller identifier used by the VM device registry.
    fn id(&self) -> InterruptControllerId;

    /// Opens one controller input using the planned trigger semantics.
    ///
    /// Implementations must return shared input state for repeated calls that
    /// address the same input. A conflicting trigger request must fail.
    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTrigger,
    ) -> IrqResult<WiredIrqInput>;
}
