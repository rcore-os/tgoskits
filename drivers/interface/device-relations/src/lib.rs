//! Discovery-time relations between devices already identified by `rdrive`.
//!
//! This crate does not allocate device identities and is not a device-lifecycle
//! manager. It records facts derived by existing discovery and binding paths.
//! The initial relation is an FDT device's interrupt controller.

#![no_std]

extern crate alloc;

pub use rdrive::DeviceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationError {
    MissingInterruptParent { device: DeviceId },
    InterruptParentMismatch {
        device: DeviceId,
        discovered: DeviceId,
        requested: DeviceId,
    },
}

/// A lightweight view of relations observed during discovery and binding.
///
/// Its validity is bounded by the current rdrive device-instance lifetime. It
/// deliberately does not claim hotplug, unbind, or invalidation semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceRelationView {
    device: DeviceId,
    interrupt_parent: Option<DeviceId>,
}

impl DeviceRelationView {
    /// Builds a view from a device identity and its rdrive-discovered FDT parent.
    pub const fn from_fdt_interrupt_parent(
        device: DeviceId,
        interrupt_parent: Option<DeviceId>,
    ) -> Self {
        Self {
            device,
            interrupt_parent,
        }
    }

    /// Validates that a binding uses the interrupt controller discovered for
    /// this device and returns that controller identity on success.
    pub fn require_interrupt_parent(
        &self,
        requested: DeviceId,
    ) -> Result<DeviceId, RelationError> {
        let Some(discovered) = self.interrupt_parent else {
            return Err(RelationError::MissingInterruptParent {
                device: self.device,
            });
        };
        if discovered != requested {
            return Err(RelationError::InterruptParentMismatch {
                device: self.device,
                discovered,
                requested,
            });
        }
        Ok(discovered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_a_requested_parent_against_the_discovered_identity() {
        let device = DeviceId::from(7);
        let controller = DeviceId::from(3);
        let view = DeviceRelationView::from_fdt_interrupt_parent(device, Some(controller));
        assert_eq!(view.require_interrupt_parent(controller), Ok(controller));
        assert_eq!(
            view.require_interrupt_parent(DeviceId::from(4)),
            Err(RelationError::InterruptParentMismatch {
                device,
                discovered: controller,
                requested: DeviceId::from(4),
            })
        );
    }
}
