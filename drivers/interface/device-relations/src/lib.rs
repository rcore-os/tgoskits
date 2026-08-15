//! Typed relations between physical devices and driver resources.
//!
//! A record is the authoritative description of one physical endpoint; a
//! relation is an explicit, validated edge between two records. Driver handles
//! remain outside this crate, so a logical runtime graph cannot be confused
//! with hardware ownership or DMA lifetime.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Stable identity assigned by the platform's device discovery layer.
pub type DeviceId = u32;

/// The physical role of a registered endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Camera,
    DmaEngine,
    Npu,
    MotionController,
    Servo,
    Wheel,
}

/// A typed dependency or data/control-flow edge between devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    CameraUsesDma,
    DmaFeedsNpu,
    ControllerDrivesServo,
    ServoDrivesWheel,
    Feedback,
}

/// Authoritative physical-device record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceRecord {
    pub id: DeviceId,
    pub kind: DeviceKind,
    /// Capability bits supplied by the platform-specific discovery layer.
    pub capabilities: u32,
}

/// Immutable relation view returned by registry queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceRelation {
    pub from: DeviceId,
    pub to: DeviceId,
    pub kind: RelationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    DuplicateDevice,
    MissingEndpoint,
    DuplicateRelation,
    InvalidRelation,
}

/// Allocation-backed registry suitable for platform discovery and driver setup.
#[derive(Debug, Default)]
pub struct DeviceRelationRegistry {
    records: Vec<DeviceRecord>,
    relations: Vec<DeviceRelation>,
}

impl DeviceRelationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, record: DeviceRecord) -> Result<(), RegistryError> {
        if self.record(record.id).is_some() {
            return Err(RegistryError::DuplicateDevice);
        }
        self.records.push(record);
        Ok(())
    }

    pub fn link(&mut self, relation: DeviceRelation) -> Result<(), RegistryError> {
        let from = self
            .record(relation.from)
            .ok_or(RegistryError::MissingEndpoint)?;
        let to = self
            .record(relation.to)
            .ok_or(RegistryError::MissingEndpoint)?;
        if !relation_is_valid(from.kind, to.kind, relation.kind) {
            return Err(RegistryError::InvalidRelation);
        }
        if self.relations.contains(&relation) {
            return Err(RegistryError::DuplicateRelation);
        }
        self.relations.push(relation);
        Ok(())
    }

    pub fn record(&self, id: DeviceId) -> Option<DeviceRecord> {
        self.records.iter().copied().find(|record| record.id == id)
    }

    pub fn relations_from(&self, id: DeviceId) -> impl Iterator<Item = DeviceRelation> + '_ {
        self.relations
            .iter()
            .copied()
            .filter(move |edge| edge.from == id)
    }
}

fn relation_is_valid(from: DeviceKind, to: DeviceKind, relation: RelationKind) -> bool {
    matches!(
        (from, to, relation),
        (
            DeviceKind::Camera,
            DeviceKind::DmaEngine,
            RelationKind::CameraUsesDma
        ) | (
            DeviceKind::DmaEngine,
            DeviceKind::Npu,
            RelationKind::DmaFeedsNpu
        ) | (
            DeviceKind::MotionController,
            DeviceKind::Servo,
            RelationKind::ControllerDrivesServo
        ) | (
            DeviceKind::Servo,
            DeviceKind::Wheel,
            RelationKind::ServoDrivesWheel
        ) | (_, _, RelationKind::Feedback)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: DeviceId, kind: DeviceKind) -> DeviceRecord {
        DeviceRecord {
            id,
            kind,
            capabilities: 0,
        }
    }

    #[test]
    fn camera_dma_npu_chain_is_typed_and_queryable() {
        let mut registry = DeviceRelationRegistry::new();
        registry.register(record(1, DeviceKind::Camera)).unwrap();
        registry.register(record(2, DeviceKind::DmaEngine)).unwrap();
        registry.register(record(3, DeviceKind::Npu)).unwrap();
        registry
            .link(DeviceRelation {
                from: 1,
                to: 2,
                kind: RelationKind::CameraUsesDma,
            })
            .unwrap();
        registry
            .link(DeviceRelation {
                from: 2,
                to: 3,
                kind: RelationKind::DmaFeedsNpu,
            })
            .unwrap();

        assert_eq!(registry.relations_from(2).count(), 1);
    }

    #[test]
    fn servo_wheel_chain_rejects_invalid_physical_edge() {
        let mut registry = DeviceRelationRegistry::new();
        registry
            .register(record(1, DeviceKind::MotionController))
            .unwrap();
        registry.register(record(2, DeviceKind::Servo)).unwrap();
        registry.register(record(3, DeviceKind::Wheel)).unwrap();

        assert_eq!(
            registry.link(DeviceRelation {
                from: 1,
                to: 3,
                kind: RelationKind::ControllerDrivesServo,
            }),
            Err(RegistryError::InvalidRelation)
        );
        registry
            .link(DeviceRelation {
                from: 1,
                to: 2,
                kind: RelationKind::ControllerDrivesServo,
            })
            .unwrap();
        registry
            .link(DeviceRelation {
                from: 2,
                to: 3,
                kind: RelationKind::ServoDrivesWheel,
            })
            .unwrap();
    }

    #[test]
    fn rejects_duplicate_ids_and_missing_endpoints() {
        let mut registry = DeviceRelationRegistry::new();
        registry.register(record(1, DeviceKind::Camera)).unwrap();
        assert_eq!(
            registry.register(record(1, DeviceKind::Camera)),
            Err(RegistryError::DuplicateDevice)
        );
        assert_eq!(
            registry.link(DeviceRelation {
                from: 1,
                to: 99,
                kind: RelationKind::CameraUsesDma,
            }),
            Err(RegistryError::MissingEndpoint)
        );
    }
}
