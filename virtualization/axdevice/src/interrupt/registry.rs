//! Validated controller and endpoint indices owned by `DeviceRuntime`.

use alloc::{collections::BTreeMap, format, string::String, sync::Arc, vec::Vec};

use axdevice_base::*;

use super::*;
use crate::*;

/// Structured runtime controller/endpoint registration failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InterruptRegistrationError {
    /// A bundle or runtime already contains the controller ID.
    #[error("interrupt controller {controller:?} is already registered")]
    DuplicateController {
        /// Duplicate VM-local ID.
        controller: InterruptControllerId,
    },
    /// A capability reports an ID different from its registration.
    #[error("{capability} capability registered as controller {registered:?} reports {reported:?}")]
    ControllerIdMismatch {
        /// ID used by the bundle.
        registered: InterruptControllerId,
        /// ID returned by the capability.
        reported: InterruptControllerId,
        /// Capability kind.
        capability: &'static str,
    },
    /// An endpoint refers to a controller absent from the runtime and bundle.
    #[error("interrupt endpoint owned by {owner} refers to missing controller {controller:?}")]
    MissingController {
        /// Missing VM-local ID.
        controller: InterruptControllerId,
        /// Device owning the endpoint claim.
        owner: String,
    },
    /// An MSI endpoint refers to a controller without message capability.
    #[error("MSI endpoint owned by {owner} requires message capability on {controller:?}")]
    MissingMessageCapability {
        /// Controller lacking MSI support.
        controller: InterruptControllerId,
        /// Device owning the endpoint claim.
        owner: String,
    },
    /// Endpoint metadata differs from the consumed planner claim.
    #[error("interrupt endpoint for {owner} slot {slot} does not match its resource claim")]
    ClaimMismatch {
        /// Device owning the claim.
        owner: String,
        /// Model-defined slot.
        slot: String,
    },
    /// Wired endpoint sharing is incompatible with an existing owner.
    #[error(
        "controller {controller:?} input {input:?} requested by {requester} conflicts with \
         {existing_owner}: {detail}"
    )]
    WiredConflict {
        /// Controller namespace.
        controller: InterruptControllerId,
        /// Controller-local input.
        input: ControllerInputId,
        /// Existing owner.
        existing_owner: String,
        /// New requester.
        requester: String,
        /// Compatibility failure.
        detail: &'static str,
    },
}

struct ControllerEntry {
    wired: Arc<dyn VirtualInterruptController>,
    message: Option<Arc<dyn MessageInterruptController>>,
}

#[derive(Clone)]
struct WiredPolicy {
    trigger: InterruptTrigger,
    sharing: InterruptSharing,
    owner: String,
}

#[derive(Default)]
struct EndpointIndex {
    wired: BTreeMap<(InterruptControllerId, ControllerInputId), WiredPolicy>,
    messages: Vec<ResolvedMsi>,
    registrations: Vec<EndpointRegistration>,
}

#[derive(Default)]
pub(crate) struct InterruptRegistry {
    controllers: BTreeMap<InterruptControllerId, ControllerEntry>,
    endpoints: EndpointIndex,
}

impl InterruptRegistry {
    pub(crate) const fn new() -> Self {
        Self {
            controllers: BTreeMap::new(),
            endpoints: EndpointIndex {
                wired: BTreeMap::new(),
                messages: Vec::new(),
                registrations: Vec::new(),
            },
        }
    }

    pub(crate) fn validate_bundle(
        &self,
        resources: &PlannedBundleResources,
    ) -> Result<(), InterruptRegistrationError> {
        self.validate_controllers(&resources.controllers)?;
        for (position, endpoint) in resources.endpoints.iter().enumerate() {
            self.validate_endpoint(
                endpoint,
                &resources.controllers,
                &resources.endpoints[..position],
            )?;
        }
        Ok(())
    }

    pub(crate) fn append(
        &mut self,
        controllers: Vec<ControllerRegistration>,
        endpoints: Vec<EndpointRegistration>,
    ) {
        for controller in controllers {
            self.controllers.insert(
                controller.id,
                ControllerEntry {
                    wired: controller.wired,
                    message: controller.message,
                },
            );
        }
        for endpoint in endpoints {
            match &endpoint {
                EndpointRegistration::Wired(registration) => {
                    self.endpoints
                        .wired
                        .entry(wired_key(registration.resolved))
                        .or_insert_with(|| WiredPolicy {
                            trigger: registration.resolved.trigger(),
                            sharing: registration.resolved.sharing(),
                            owner: registration.lease.device_id().into(),
                        });
                }
                EndpointRegistration::Message(registration) => {
                    self.endpoints.messages.push(registration.resolved);
                }
            }
            self.endpoints.registrations.push(endpoint);
        }
    }

    pub(crate) fn wired_controller(
        &self,
        id: InterruptControllerId,
    ) -> DeviceManagerResult<Arc<dyn VirtualInterruptController>> {
        self.controllers
            .get(&id)
            .map(|entry| entry.wired.clone())
            .ok_or_else(|| missing_controller_error(id))
    }

    pub(crate) fn message_controller(
        &self,
        id: InterruptControllerId,
    ) -> DeviceManagerResult<Arc<dyn MessageInterruptController>> {
        let entry = self
            .controllers
            .get(&id)
            .ok_or_else(|| missing_controller_error(id))?;
        entry
            .message
            .clone()
            .ok_or_else(|| DeviceManagerError::Unsupported {
                operation: "resolve planned MSI endpoint",
                detail: format!("controller {} has no message capability", id.value()),
            })
    }

    fn validate_controllers(
        &self,
        incoming: &[ControllerRegistration],
    ) -> Result<(), InterruptRegistrationError> {
        for (position, controller) in incoming.iter().enumerate() {
            if controller.wired.id() != controller.id {
                return Err(InterruptRegistrationError::ControllerIdMismatch {
                    registered: controller.id,
                    reported: controller.wired.id(),
                    capability: "wired",
                });
            }
            if let Some(message) = &controller.message
                && message.id() != controller.id
            {
                return Err(InterruptRegistrationError::ControllerIdMismatch {
                    registered: controller.id,
                    reported: message.id(),
                    capability: "message",
                });
            }
            if self.controllers.contains_key(&controller.id)
                || incoming[..position]
                    .iter()
                    .any(|existing| existing.id == controller.id)
            {
                return Err(InterruptRegistrationError::DuplicateController {
                    controller: controller.id,
                });
            }
        }
        Ok(())
    }

    fn validate_endpoint(
        &self,
        endpoint: &EndpointRegistration,
        incoming_controllers: &[ControllerRegistration],
        earlier: &[EndpointRegistration],
    ) -> Result<(), InterruptRegistrationError> {
        match endpoint {
            EndpointRegistration::Wired(registration) => {
                self.validate_wired(registration, incoming_controllers, earlier)
            }
            EndpointRegistration::Message(registration) => {
                self.validate_message(registration, incoming_controllers)
            }
        }
    }

    fn validate_wired(
        &self,
        registration: &WiredEndpointRegistration,
        incoming_controllers: &[ControllerRegistration],
        earlier: &[EndpointRegistration],
    ) -> Result<(), InterruptRegistrationError> {
        let resolved = registration.resolved;
        if registration.lease.wired_irq().ok() != Some(resolved) {
            return Err(claim_mismatch(
                registration.lease.device_id(),
                registration.lease.slot(),
            ));
        }
        self.require_controller(
            resolved.controller(),
            registration.lease.device_id(),
            incoming_controllers,
            false,
        )?;

        let key = wired_key(resolved);
        let earlier_policy = earlier.iter().find_map(|endpoint| match endpoint {
            EndpointRegistration::Wired(earlier) if wired_key(earlier.resolved) == key => {
                Some(WiredPolicy {
                    trigger: earlier.resolved.trigger(),
                    sharing: earlier.resolved.sharing(),
                    owner: earlier.lease.device_id().into(),
                })
            }
            _ => None,
        });
        if let Some(existing) = self.endpoints.wired.get(&key).or(earlier_policy.as_ref()) {
            validate_sharing(existing, resolved, registration.lease.device_id())?;
        }
        Ok(())
    }

    fn validate_message(
        &self,
        registration: &MessageEndpointRegistration,
        incoming_controllers: &[ControllerRegistration],
    ) -> Result<(), InterruptRegistrationError> {
        let resolved = registration.resolved;
        if registration.lease.msi().ok() != Some(resolved) {
            return Err(claim_mismatch(
                registration.lease.device_id(),
                registration.lease.slot(),
            ));
        }
        self.require_controller(
            resolved.controller(),
            registration.lease.device_id(),
            incoming_controllers,
            true,
        )
    }

    fn require_controller(
        &self,
        controller: InterruptControllerId,
        owner: &str,
        incoming: &[ControllerRegistration],
        message: bool,
    ) -> Result<(), InterruptRegistrationError> {
        let existing = self.controllers.get(&controller);
        let incoming = incoming.iter().find(|candidate| candidate.id == controller);
        if existing.is_none() && incoming.is_none() {
            return Err(InterruptRegistrationError::MissingController {
                controller,
                owner: owner.into(),
            });
        }
        let has_message = existing.is_some_and(|entry| entry.message.is_some())
            || incoming.is_some_and(|entry| entry.message.is_some());
        if message && !has_message {
            return Err(InterruptRegistrationError::MissingMessageCapability {
                controller,
                owner: owner.into(),
            });
        }
        Ok(())
    }
}

fn wired_key(resolved: ResolvedWiredIrq) -> (InterruptControllerId, ControllerInputId) {
    (resolved.controller(), resolved.input())
}

fn validate_sharing(
    existing: &WiredPolicy,
    incoming: ResolvedWiredIrq,
    requester: &str,
) -> Result<(), InterruptRegistrationError> {
    if existing.sharing == InterruptSharing::Shared
        && incoming.sharing() == InterruptSharing::Shared
        && existing.trigger == incoming.trigger()
    {
        return Ok(());
    }
    Err(InterruptRegistrationError::WiredConflict {
        controller: incoming.controller(),
        input: incoming.input(),
        existing_owner: existing.owner.clone(),
        requester: requester.into(),
        detail: if existing.trigger == incoming.trigger() {
            "an exclusive endpoint cannot share an input"
        } else {
            "shared endpoints require identical trigger semantics"
        },
    })
}

fn claim_mismatch(owner: &str, slot: &crate::ResourceSlot) -> InterruptRegistrationError {
    InterruptRegistrationError::ClaimMismatch {
        owner: owner.into(),
        slot: slot.as_str().into(),
    }
}

fn missing_controller_error(id: InterruptControllerId) -> DeviceManagerError {
    DeviceManagerError::ResourceNotFound {
        operation: "resolve virtual interrupt controller",
        resource: format!("interrupt controller {}", id.value()),
    }
}
