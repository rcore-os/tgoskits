//! RISC-V hypervisor physical-PLIC lifecycle extension.

use super::IrqError;

/// Fixed IRQ-safe ingress implemented by the hypervisor runtime.
///
/// The dynamic platform invokes this interface only after it has detached and
/// retained the matching host PLIC completion. Implementations must perform no
/// allocation, VM lookup, ordinary-lock acquisition, or subscriber callback.
#[ax_crate_interface::def_interface]
pub trait RiscvHvIrqSink {
    /// Publishes one claimed physical source to its pre-registered VM binding.
    fn publish_physical_plic_claim(source: u32) -> bool;
}

/// Publishes one detached host PLIC claim to the fixed hypervisor ingress.
#[inline]
pub fn publish_physical_plic_claim(source: u32) -> bool {
    ax_crate_interface::call_interface!(RiscvHvIrqSink::publish_physical_plic_claim, source)
}

/// Dynamic-platform operations for guest-owned physical PLIC sources.
#[def_plat_interface]
pub trait RiscvHvIrqIf {
    /// Routes and enables one physical source after the VM binding is ready.
    fn activate_guest_plic_source(source: u32, target_cpu: usize) -> Result<(), IrqError>;

    /// Disables one guest-owned physical source and restores host affinity.
    fn deactivate_guest_plic_source(source: u32) -> Result<(), IrqError>;

    /// Completes the outstanding host claim for `source`, if one exists.
    fn complete_guest_plic_source(source: u32) -> bool;
}
