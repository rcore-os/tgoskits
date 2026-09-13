//! Bundle-local interrupt registrations.

use alloc::{sync::Arc, vec::Vec};

use axdevice_base::{MessageInterruptController, VirtualInterruptController};

use crate::*;

/// A VM-local interrupt controller contributed by a device bundle.
#[derive(Clone)]
pub struct ControllerRegistration {
    pub(crate) id: axdevice_base::InterruptControllerId,
    pub(crate) wired: Arc<dyn VirtualInterruptController>,
    pub(crate) message: Option<Arc<dyn MessageInterruptController>>,
}

impl ControllerRegistration {
    /// Registers one wired-interrupt controller capability under `id`.
    pub fn new(
        id: axdevice_base::InterruptControllerId,
        wired: Arc<dyn VirtualInterruptController>,
    ) -> Self {
        Self {
            id,
            wired,
            message: None,
        }
    }

    /// Adds the optional MSI capability implemented by the same controller.
    pub fn with_message(mut self, message: Arc<dyn MessageInterruptController>) -> Self {
        self.message = Some(message);
        self
    }

    /// Returns the registered VM-local controller identifier.
    pub const fn id(&self) -> axdevice_base::InterruptControllerId {
        self.id
    }
}

pub(crate) enum EndpointRegistration {
    Wired(WiredEndpointRegistration),
    Message(MessageEndpointRegistration),
}

pub(crate) struct WiredEndpointRegistration {
    pub(crate) resolved: ResolvedWiredIrq,
    pub(crate) lease: ResourceLease,
}

pub(crate) struct MessageEndpointRegistration {
    pub(crate) resolved: ResolvedMsi,
    pub(crate) lease: ResourceLease,
}

#[derive(Default)]
pub(crate) struct PlannedBundleResources {
    pub(crate) controllers: Vec<ControllerRegistration>,
    pub(crate) endpoints: Vec<EndpointRegistration>,
    pub(crate) leases: Vec<ResourceLease>,
}

impl PlannedBundleResources {
    pub(crate) const fn new() -> Self {
        Self {
            controllers: Vec::new(),
            endpoints: Vec::new(),
            leases: Vec::new(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.controllers.is_empty() && self.endpoints.is_empty() && self.leases.is_empty()
    }
}
