//! Planned resources retained by a sealed device runtime.

use alloc::vec::Vec;

use crate::{interrupt::*, *};

#[derive(Default)]
pub(crate) struct PlannedRuntimeResources {
    pub(crate) interrupts: InterruptRegistry,
    leases: Vec<ResourceLease>,
}

#[derive(Clone)]
pub(crate) struct PlannedRuntimeCheckpoint {
    interrupts: InterruptRegistryCheckpoint,
    leases_len: usize,
}

impl PlannedRuntimeResources {
    pub(crate) const fn new() -> Self {
        Self {
            interrupts: InterruptRegistry::new(),
            leases: Vec::new(),
        }
    }

    pub(crate) fn validate_bundle(
        &self,
        resources: &PlannedBundleResources,
    ) -> DeviceManagerResult {
        self.interrupts.validate_bundle(resources)?;
        Ok(())
    }

    pub(crate) fn append(&mut self, resources: PlannedBundleResources) {
        self.interrupts
            .append(resources.controllers, resources.endpoints);
        self.leases.extend(resources.leases);
    }

    pub(crate) fn checkpoint(&self) -> PlannedRuntimeCheckpoint {
        PlannedRuntimeCheckpoint {
            interrupts: self.interrupts.checkpoint(),
            leases_len: self.leases.len(),
        }
    }

    pub(crate) fn rollback(&mut self, checkpoint: PlannedRuntimeCheckpoint) {
        self.interrupts.rollback(checkpoint.interrupts);
        self.leases.truncate(checkpoint.leases_len);
    }
}
