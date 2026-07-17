//! x86 IOAPIC passthrough forwarding orchestration.
//!
//! Configuration records host/guest route identity, activation transfers the
//! route to a guest only after the vIOAPIC is ready, the hard-IRQ handler
//! publishes pending work, and revocation masks and drains the old owner.

mod activation;
mod handler;
mod revocation;
mod state;

pub use activation::{
    activate_ready_ioapic_forwarding_routes, enable_ioapic_irq_forwarding,
    register_ioapic_irq_forwarding_activation, reserve_ioapic_irq_forwarding_action,
};
pub use handler::{
    drain_bound_pending_ioapic_irqs, queue_due_pit_irq0, queue_pending_ioapic_irq_after_eoi,
    queue_pending_serial_irq,
};
pub use revocation::disable_ioapic_irq_forwarding_for_vm;
#[cfg(any(feature = "fs", feature = "host-fs"))]
pub use revocation::revoke_ioapic_irq_forwarding_for_vm;
pub use state::{
    IoApicForwardingActivationOps, register_ioapic_irq_forwarding_route,
    register_ioapic_irq_forwarding_route_with_trigger,
};

#[cfg(test)]
mod tests;
