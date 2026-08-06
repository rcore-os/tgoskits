//! Planned resources retained by a sealed device runtime.

use alloc::vec::Vec;

use crate::{interrupt::*, *};

#[derive(Default)]
pub(crate) struct PlannedRuntimeResources {
    pub(crate) interrupts: InterruptRegistry,
    leases: Vec<ResourceLease>,
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
}
