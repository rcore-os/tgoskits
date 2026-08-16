//! Discovery-time relations between devices already identified by `rdrive`.
//!
//! This crate does not allocate device identities and is not a device-lifecycle
//! manager. It records facts derived by existing discovery and binding paths.
//! The initial relation is an FDT device's interrupt controller.

#![no_std]

extern crate alloc;

pub use rdrive::DeviceId;

/// A lightweight view of relations observed during discovery and binding.
///
/// Its validity is bounded by the current rdrive device-instance lifetime. It
/// deliberately does not claim hotplug, unbind, or invalidation semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceRelationView {
    device: DeviceId,
    interrupt_parent: DeviceId,
}

impl DeviceRelationView {
    /// Builds a view from an interrupt parent already parsed by rdrive.
    ///
    /// The parent may originate from either `interrupt-parent` or an
    /// `interrupts-extended` specifier; this view deliberately does not
    /// reinterpret the FDT syntax that produced it.
    pub const fn from_fdt_interrupt(device: DeviceId, interrupt_parent: DeviceId) -> Self {
        Self {
            device,
            interrupt_parent,
        }
    }

    /// Returns the parsed controller identity for this interrupt relation.
    pub const fn interrupt_parent(&self) -> DeviceId {
        self.interrupt_parent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_the_parsed_interrupt_parent_identity() {
        let device = DeviceId::from(7);
        let controller = DeviceId::from(3);
        let view = DeviceRelationView::from_fdt_interrupt(device, controller);
        assert_eq!(view.interrupt_parent(), controller);
    }
}
