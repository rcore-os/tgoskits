//! Interrupt topology registration, validation, and connection flow.

use alloc::{format, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_kspin::SpinRaw;
use axdevice_base::{InterruptControllerId, InterruptEndpoint, IrqError, Resource, WiredIrqInput};
use axvm_types::InterruptDelivery;

use super::{
    ControllerRef, ControllerRegistration, ControllerRole, InterruptClaimDomain,
    InterruptEndpointRegistration, InterruptPlanAuthority, MsiClaim, PlannedIrqConnection,
    PlannedMsiConnection, VcpuInterruptBinding, VcpuInterruptId, VcpuInterruptPort, WiredIrqClaim,
    WiredIrqRequest,
    registry::{ControllerEntry, find_controller, find_controller_mut, registration_order},
};
use crate::{DeviceManagerError, DeviceManagerResult};

/// One VM's validated interrupt-controller graph.
pub struct InterruptTopology {
    delivery: InterruptDelivery,
    controllers: SpinRaw<Vec<ControllerEntry>>,
    bindings: SpinRaw<Vec<RegisteredBinding>>,
    connected_cascades: SpinRaw<Vec<InterruptControllerId>>,
    connected_cascade_claims: SpinRaw<Vec<InterruptEndpointRegistration>>,
    claim_domain: Arc<InterruptClaimDomain>,
    finalized: AtomicBool,
}

struct RegisteredBinding {
    vcpu: VcpuInterruptId,
    binding: Arc<dyn VcpuInterruptBinding>,
}

impl InterruptTopology {
    /// Creates an empty topology for one normalized delivery policy.
    pub fn new(delivery: InterruptDelivery) -> (Self, InterruptPlanAuthority) {
        let claim_domain = Arc::new(InterruptClaimDomain::new());
        let authority = InterruptPlanAuthority::new(claim_domain.clone());
        (
            Self {
                delivery,
                controllers: SpinRaw::new(Vec::new()),
                bindings: SpinRaw::new(Vec::new()),
                connected_cascades: SpinRaw::new(Vec::new()),
                connected_cascade_claims: SpinRaw::new(Vec::new()),
                claim_domain,
                finalized: AtomicBool::new(false),
            },
            authority,
        )
    }

    /// Returns the configured external interrupt-delivery policy.
    pub const fn delivery(&self) -> InterruptDelivery {
        self.delivery
    }

    /// Registers one controller before topology finalization.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate IDs, multiple default controllers, a
    /// controller without capabilities, or registration after finalization.
    pub fn register_controller(&self, registration: ControllerRegistration) -> DeviceManagerResult {
        if self.finalized.load(Ordering::Acquire) {
            return Err(DeviceManagerError::InvalidInput {
                operation: "register interrupt controller",
                detail: "the interrupt topology is already finalized".into(),
            });
        }
        if registration.wired_inputs().is_none()
            && registration.message_inputs().is_none()
            && registration.vcpu_controller().is_none()
        {
            return Err(DeviceManagerError::InvalidInput {
                operation: "register interrupt controller",
                detail: format!("controller {:?} exposes no capabilities", registration.id()),
            });
        }

        let mut controllers = self.controllers.lock();
        if controllers
            .iter()
            .any(|entry| entry.registration.id() == registration.id())
        {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "register interrupt controller",
                detail: format!("controller {:?} is already registered", registration.id()),
            });
        }
        if registration.role() == ControllerRole::Default
            && controllers
                .iter()
                .any(|entry| entry.registration.role() == ControllerRole::Default)
        {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "register interrupt controller",
                detail: "a default interrupt controller is already registered".into(),
            });
        }
        controllers.push(ControllerEntry {
            registration,
            wired_inputs: Vec::new(),
        });
        Ok(())
    }

    /// Connects one planner-authorized source to a wired controller input.
    ///
    /// # Errors
    ///
    /// Returns an error if the claim belongs to another topology, was already
    /// consumed, or the controller rejects the requested input or trigger.
    pub fn connect_irq(&self, claim: WiredIrqClaim) -> DeviceManagerResult<PlannedIrqConnection> {
        self.require_claim_domain(claim.domain())?;
        let Resource::WiredIrq {
            controller: controller_id,
            input,
            trigger,
            sharing,
        } = *claim.resource()
        else {
            return Err(DeviceManagerError::InvalidInput {
                operation: "connect planned wired interrupt",
                detail: "wired claim contains a different resource kind".into(),
            });
        };
        claim.mark_connected()?;
        let request = WiredIrqRequest::for_controller(controller_id, input, trigger, sharing);
        let input = self.open_wired_input(controller_id, request)?;
        let line = input.connect()?;
        Ok(PlannedIrqConnection::new(line, claim.into_registration()))
    }

    /// Connects one planner-authorized MSI event to a controller.
    ///
    /// # Errors
    ///
    /// Returns an error if the claim belongs to another topology, was already
    /// consumed, or the controller rejects the requested event.
    pub fn connect_msi(&self, claim: MsiClaim) -> DeviceManagerResult<PlannedMsiConnection> {
        self.require_claim_domain(claim.domain())?;
        let Resource::MessageInterrupt {
            controller: controller_id,
            device,
            event,
        } = *claim.resource()
        else {
            return Err(DeviceManagerError::InvalidInput {
                operation: "connect planned message interrupt",
                detail: "MSI claim contains a different resource kind".into(),
            });
        };
        claim.mark_connected()?;
        let capability = {
            let controllers = self.controllers.lock();
            let entry = find_controller(&controllers, controller_id)?;
            entry
                .registration
                .message_inputs()
                .cloned()
                .ok_or_else(|| DeviceManagerError::Unsupported {
                    operation: "connect message interrupt",
                    detail: format!("controller {controller_id:?} has no MSI input capability"),
                })?
        };
        let endpoint = capability.connect(device, event)?;
        if endpoint.controller() != controller_id
            || endpoint.message().device() != device
            || endpoint.message().event() != event
        {
            return Err(DeviceManagerError::InvalidInput {
                operation: "connect message interrupt",
                detail: "controller returned an endpoint for a different MSI request".into(),
            });
        }
        Ok(PlannedMsiConnection::new(
            endpoint,
            claim.into_registration(),
        ))
    }

    /// Validates cascades, connects controller outputs, and attaches vCPUs.
    pub fn finalize(&self, ports: &[VcpuInterruptPort]) -> DeviceManagerResult {
        if self
            .finalized
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(DeviceManagerError::InvalidInput {
                operation: "finalize interrupt topology",
                detail: "the topology is already finalized".into(),
            });
        }

        let result = self.finalize_inner(ports);
        if result.is_err() {
            self.rollback_finalization();
        }
        result
    }

    /// Removes every capability installed during a failed VM preparation.
    ///
    /// This operation disconnects controller cascades before dropping vCPU
    /// bindings and controller registrations, so the same topology object can
    /// be configured again after the caller has rebuilt its device set.
    ///
    /// # Errors
    ///
    /// Returns the first controller-output disconnection failure after still
    /// clearing all topology-owned registrations.
    pub fn reset_after_failed_preparation(&self) -> DeviceManagerResult {
        let disconnect_result = self.disconnect_cascades();
        self.bindings.lock().clear();
        self.controllers.lock().clear();
        self.finalized.store(false, Ordering::Release);
        disconnect_result
    }

    /// Restores all controller bindings associated with `vcpu`.
    pub fn load_vcpu(&self, vcpu: VcpuInterruptId) -> DeviceManagerResult {
        for binding in self.bindings_for(vcpu) {
            binding.load()?;
        }
        Ok(())
    }

    /// Saves all controller bindings associated with `vcpu` in reverse order.
    pub fn save_vcpu(&self, vcpu: VcpuInterruptId) -> DeviceManagerResult {
        let mut bindings = self.bindings_for(vcpu);
        bindings.reverse();
        for binding in bindings {
            binding.save()?;
        }
        Ok(())
    }

    /// Reconciles completed and pending controller work for `vcpu`.
    pub fn synchronize_vcpu(&self, vcpu: VcpuInterruptId) -> DeviceManagerResult {
        for binding in self.bindings_for(vcpu) {
            binding.synchronize()?;
        }
        Ok(())
    }

    /// Returns whether the topology has completed cascade and vCPU binding.
    pub fn is_finalized(&self) -> bool {
        self.finalized.load(Ordering::Acquire)
    }

    /// Returns a snapshot of all endpoint reservations currently held in this
    /// topology, including device, controller-cascade, and architecture
    /// infrastructure leases.
    pub fn active_endpoint_resources(&self) -> Vec<Resource> {
        self.claim_domain.resources()
    }

    pub(crate) fn unregister_controller(
        &self,
        controller: InterruptControllerId,
    ) -> DeviceManagerResult {
        if self.finalized.load(Ordering::Acquire) {
            return Err(DeviceManagerError::InvalidInput {
                operation: "unregister interrupt controller",
                detail: "the interrupt topology is already finalized".into(),
            });
        }
        let mut controllers = self.controllers.lock();
        let Some(index) = controllers
            .iter()
            .position(|entry| entry.registration.id() == controller)
        else {
            return Err(DeviceManagerError::ResourceNotFound {
                operation: "unregister interrupt controller",
                resource: format!("interrupt controller {controller:?}"),
            });
        };
        controllers.remove(index);
        Ok(())
    }

    fn finalize_inner(&self, ports: &[VcpuInterruptPort]) -> DeviceManagerResult {
        validate_unique_vcpu_ports(ports)?;
        let ordered_ids = registration_order(&self.controllers.lock())?;
        self.connect_cascades(&ordered_ids)?;
        self.attach_vcpus(&ordered_ids, ports)
    }

    fn connect_cascades(&self, ordered_ids: &[InterruptControllerId]) -> DeviceManagerResult {
        for controller_id in ordered_ids {
            let cascade = {
                let controllers = self.controllers.lock();
                find_controller(&controllers, *controller_id)?
                    .registration
                    .cascade()
                    .cloned()
            };
            if let Some(cascade) = cascade {
                let claim = self.claim_internal_wired(cascade.parent())?;
                let (line, registration) = self.connect_irq(claim)?.into_parts();
                cascade.connect_output(line)?;
                self.connected_cascades.lock().push(*controller_id);
                self.connected_cascade_claims.lock().push(registration);
            }
        }
        Ok(())
    }

    fn attach_vcpus(
        &self,
        ordered_ids: &[InterruptControllerId],
        ports: &[VcpuInterruptPort],
    ) -> DeviceManagerResult {
        let mut attached = Vec::new();
        for controller_id in ordered_ids {
            let capability = {
                let controllers = self.controllers.lock();
                find_controller(&controllers, *controller_id)?
                    .registration
                    .vcpu_controller()
                    .cloned()
            };
            if let Some(capability) = capability {
                for port in ports {
                    attached.push(RegisteredBinding {
                        vcpu: port.id(),
                        binding: capability.attach_vcpu(port.clone())?,
                    });
                }
            }
        }
        *self.bindings.lock() = attached;
        Ok(())
    }

    fn open_wired_input(
        &self,
        controller_id: InterruptControllerId,
        request: WiredIrqRequest,
    ) -> DeviceManagerResult<WiredIrqInput> {
        let capability = {
            let controllers = self.controllers.lock();
            let entry = find_controller(&controllers, controller_id)?;
            if let Some((_, input)) = entry
                .wired_inputs
                .iter()
                .find(|(input, _)| *input == request.input())
            {
                if input.trigger() != request.trigger() {
                    return Err(IrqError::InvalidTriggerMode {
                        endpoint: InterruptEndpoint::Wired {
                            controller: controller_id,
                            input: request.input(),
                        },
                        operation: "connect interrupt source",
                        expected: input.trigger(),
                        actual: request.trigger(),
                    }
                    .into());
                }
                return Ok(input.clone());
            }
            entry.registration.wired_inputs().cloned().ok_or_else(|| {
                DeviceManagerError::Unsupported {
                    operation: "connect wired interrupt",
                    detail: format!("controller {controller_id:?} has no wired inputs"),
                }
            })?
        };

        let opened = capability.input(request.input(), request.trigger())?;
        if opened.controller() != controller_id
            || opened.input() != request.input()
            || opened.trigger() != request.trigger()
        {
            return Err(DeviceManagerError::InvalidInput {
                operation: "connect wired interrupt",
                detail: "controller returned an input for a different request".into(),
            });
        }

        let mut controllers = self.controllers.lock();
        let entry = find_controller_mut(&mut controllers, controller_id)?;
        if let Some((_, existing)) = entry
            .wired_inputs
            .iter()
            .find(|(input, _)| *input == request.input())
        {
            return Ok(existing.clone());
        }
        entry.wired_inputs.push((request.input(), opened.clone()));
        Ok(opened)
    }

    pub(super) fn resolve_controller(
        &self,
        reference: ControllerRef,
    ) -> DeviceManagerResult<InterruptControllerId> {
        let controllers = self.controllers.lock();
        super::registry::resolve_controller(&controllers, reference)
    }

    pub(super) fn claim_domain(&self) -> &Arc<InterruptClaimDomain> {
        &self.claim_domain
    }

    pub(super) fn require_claim_domain(
        &self,
        domain: &Arc<InterruptClaimDomain>,
    ) -> DeviceManagerResult {
        if Arc::ptr_eq(&self.claim_domain, domain) {
            Ok(())
        } else {
            Err(DeviceManagerError::InvalidInput {
                operation: "connect planned interrupt endpoint",
                detail: "interrupt claim belongs to a different VM topology".into(),
            })
        }
    }

    fn claim_internal_wired(&self, request: WiredIrqRequest) -> DeviceManagerResult<WiredIrqClaim> {
        let controller = self.resolve_controller(request.controller())?;
        self.claim_domain.claim_wired(Resource::WiredIrq {
            controller,
            input: request.input(),
            trigger: request.trigger(),
            sharing: request.sharing(),
        })
    }

    fn bindings_for(&self, vcpu: VcpuInterruptId) -> Vec<Arc<dyn VcpuInterruptBinding>> {
        self.bindings
            .lock()
            .iter()
            .filter(|registered| registered.vcpu == vcpu)
            .map(|registered| registered.binding.clone())
            .collect()
    }

    fn rollback_finalization(&self) {
        if let Err(error) = self.disconnect_cascades() {
            warn!("failed to roll back interrupt-controller cascades: {error}");
        }
        self.bindings.lock().clear();
        self.finalized.store(false, Ordering::Release);
    }

    fn disconnect_cascades(&self) -> DeviceManagerResult {
        let connected = core::mem::take(&mut *self.connected_cascades.lock());
        let claims = core::mem::take(&mut *self.connected_cascade_claims.lock());
        let cascades = {
            let controllers = self.controllers.lock();
            let mut cascades = Vec::with_capacity(connected.len());
            for controller_id in connected.into_iter().rev() {
                let cascade = find_controller(&controllers, controller_id)?
                    .registration
                    .cascade()
                    .cloned()
                    .ok_or_else(|| DeviceManagerError::InvalidConfig {
                        operation: "disconnect interrupt-controller cascade",
                        detail: format!("controller {controller_id:?} lost its registered cascade"),
                    })?;
                cascades.push(cascade);
            }
            cascades
        };

        let mut first_error = None;
        for cascade in cascades {
            if let Err(error) = cascade.disconnect_output()
                && first_error.is_none()
            {
                first_error = Some(DeviceManagerError::from(error));
            }
        }
        drop(claims);
        first_error.map_or(Ok(()), Err)
    }
}

impl core::fmt::Debug for InterruptTopology {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InterruptTopology")
            .field("delivery", &self.delivery)
            .field("controller_count", &self.controllers.lock().len())
            .field("binding_count", &self.bindings.lock().len())
            .field("cascade_count", &self.connected_cascades.lock().len())
            .field("finalized", &self.is_finalized())
            .finish()
    }
}

impl Drop for InterruptTopology {
    fn drop(&mut self) {
        if let Err(error) = self.disconnect_cascades() {
            warn!(
                "failed to disconnect interrupt-controller cascades during topology drop: {error}"
            );
        }
    }
}

fn validate_unique_vcpu_ports(ports: &[VcpuInterruptPort]) -> DeviceManagerResult {
    for (index, port) in ports.iter().enumerate() {
        if ports[..index]
            .iter()
            .any(|existing| existing.id() == port.id())
        {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "attach interrupt controller to vCPU",
                detail: format!("vCPU interrupt port {:?} is duplicated", port.id()),
            });
        }
    }
    Ok(())
}
