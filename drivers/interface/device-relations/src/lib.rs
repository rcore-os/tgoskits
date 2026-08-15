//! Discovery-time relations between devices already identified by `rdrive`.
//!
//! This crate does not allocate device identities and is not a device-lifecycle
//! manager. It records facts derived by existing discovery and binding paths.
//! The initial relation is an FDT device's interrupt controller.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub use rdrive::DeviceId;

/// A fact derived from an existing device-discovery or binding path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// The source FDT node names the target as its interrupt parent.
    InterruptParent,
}

/// A typed relation between two identities allocated by `rdrive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceRelation {
    pub device: DeviceId,
    pub provider: DeviceId,
    pub kind: RelationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationError {
    DuplicateRelation,
}

/// A lightweight view of relations observed during discovery and binding.
///
/// Its validity is bounded by the current rdrive device-instance lifetime. It
/// deliberately does not claim hotplug, unbind, or invalidation semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceRelationView {
    relations: Vec<DeviceRelation>,
}

impl DeviceRelationView {
    pub const fn new() -> Self {
        Self {
            relations: Vec::new(),
        }
    }

    pub fn record_interrupt_parent(
        &mut self,
        device: DeviceId,
        provider: DeviceId,
    ) -> Result<(), RelationError> {
        let relation = DeviceRelation {
            device,
            provider,
            kind: RelationKind::InterruptParent,
        };
        if self.relations.contains(&relation) {
            return Err(RelationError::DuplicateRelation);
        }
        self.relations.push(relation);
        Ok(())
    }

    pub fn interrupt_parent(&self, device: DeviceId) -> Option<DeviceId> {
        self.relations
            .iter()
            .find(|relation| {
                relation.device == device && relation.kind == RelationKind::InterruptParent
            })
            .map(|relation| relation.provider)
    }

    pub fn relations_from(&self, device: DeviceId) -> impl Iterator<Item = DeviceRelation> + '_ {
        self.relations
            .iter()
            .copied()
            .filter(move |relation| relation.device == device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_rdrive_identities_and_rejects_duplicate_facts() {
        let device = DeviceId::from(7);
        let controller = DeviceId::from(3);
        let mut view = DeviceRelationView::new();
        view.record_interrupt_parent(device, controller).unwrap();
        assert_eq!(view.interrupt_parent(device), Some(controller));
        assert_eq!(view.relations_from(device).count(), 1);
        assert_eq!(
            view.record_interrupt_parent(device, controller),
            Err(RelationError::DuplicateRelation)
        );
    }
}
