//! RISC-V hypervisor physical-external-IRQ lifecycle extension.

use super::{ControllerIrqTrigger, IrqError};

/// Fixed IRQ-safe ingress implemented by the hypervisor runtime.
///
/// The dynamic platform invokes this interface only after it has detached and
/// retained the matching host external IRQ completion. Implementations must perform no
/// allocation, VM lookup, ordinary-lock acquisition, or subscriber callback.
#[ax_crate_interface::def_interface]
pub trait RiscvHvIrqSink {
    /// Publishes one claimed physical source to its pre-registered VM binding.
    fn publish_physical_external_claim(source: u32) -> bool;
}

/// Publishes one detached host external IRQ claim to the fixed hypervisor ingress.
#[inline]
pub fn publish_physical_external_claim(source: u32) -> bool {
    ax_crate_interface::call_interface!(RiscvHvIrqSink::publish_physical_external_claim, source)
}

/// Dynamic-platform operations for guest-owned physical external IRQ sources.
#[def_plat_interface]
pub trait RiscvHvIrqIf {
    /// Configures, routes and enables a source after its VM binding is ready.
    /// APLIC requires a firmware trigger and a source not retired by a previous owner.
    fn activate_guest_external_source(
        source: u32,
        target_cpu: usize,
        trigger: Option<ControllerIrqTrigger>,
    ) -> Result<(), IrqError>;

    /// Disables a guest source. APLIC retires it until platform reset; PLIC
    /// restores host affinity. This does not prove that device DMA has stopped.
    fn deactivate_guest_external_source(source: u32) -> Result<(), IrqError>;

    /// Completes the outstanding host claim for `source`, if one exists.
    fn complete_guest_external_source(source: u32) -> bool;
}
