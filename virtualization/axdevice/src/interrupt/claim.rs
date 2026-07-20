//! Planner-issued interrupt claims and transaction-owned endpoint leases.

use alloc::{collections::BTreeMap, format, sync::Arc};

use ax_kspin::SpinRaw;
use axdevice_base::{InterruptEndpointKey, IrqLine, MsiEndpoint, Resource};

use super::{InterruptTopology, MsiRequest, WiredIrqRequest};
use crate::{DeviceManagerError, DeviceManagerResult};

/// Authority held only by the VM build transaction while it consumes an
/// immutable machine plan.
///
/// Claims issued by one authority are accepted only by the topology created
/// with that authority. Device models never receive this capability.
pub struct InterruptPlanAuthority {
    domain: Arc<InterruptClaimDomain>,
}

impl InterruptPlanAuthority {
    pub(super) const fn new(domain: Arc<InterruptClaimDomain>) -> Self {
        Self { domain }
    }

    /// Reserves one plan-described wired endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the authority belongs to another topology, the
    /// controller cannot be resolved, or the requested sharing policy
    /// conflicts with an active reservation.
    pub fn claim_wired(
        &self,
        topology: &InterruptTopology,
        request: WiredIrqRequest,
    ) -> DeviceManagerResult<WiredIrqClaim> {
        topology.require_claim_domain(&self.domain)?;
        let controller = topology.resolve_controller(request.controller())?;
        let resource = Resource::WiredIrq {
            controller,
            input: request.input(),
            trigger: request.trigger(),
            sharing: request.sharing(),
        };
        self.domain.claim_wired(resource)
    }

    /// Reserves one plan-described message-signaled endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the authority belongs to another topology, the
    /// controller cannot be resolved, or the event is already reserved.
    pub fn claim_msi(
        &self,
        topology: &InterruptTopology,
        request: MsiRequest,
    ) -> DeviceManagerResult<MsiClaim> {
        topology.require_claim_domain(&self.domain)?;
        let controller = topology.resolve_controller(request.controller())?;
        let resource = Resource::MessageInterrupt {
            controller,
            device: request.device(),
            event: request.event(),
        };
        self.domain.claim_msi(resource)
    }
}

impl core::fmt::Debug for InterruptPlanAuthority {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InterruptPlanAuthority")
            .finish_non_exhaustive()
    }
}

/// An opaque, single-use claim for one wired interrupt source.
pub struct WiredIrqClaim {
    lease: InterruptClaimLease,
}

impl WiredIrqClaim {
    pub(super) fn domain(&self) -> &Arc<InterruptClaimDomain> {
        &self.lease.domain
    }

    pub(super) fn resource(&self) -> &Resource {
        self.lease.resource()
    }

    pub(super) fn mark_connected(&self) -> DeviceManagerResult {
        self.lease.mark_connected()
    }

    pub(super) fn into_registration(self) -> InterruptEndpointRegistration {
        InterruptEndpointRegistration { lease: self.lease }
    }
}

impl core::fmt::Debug for WiredIrqClaim {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("WiredIrqClaim")
            .field("resource", self.resource())
            .finish_non_exhaustive()
    }
}

/// An opaque, single-use claim for one message-signaled event.
pub struct MsiClaim {
    lease: InterruptClaimLease,
}

impl MsiClaim {
    pub(super) fn domain(&self) -> &Arc<InterruptClaimDomain> {
        &self.lease.domain
    }

    pub(super) fn resource(&self) -> &Resource {
        self.lease.resource()
    }

    pub(super) fn mark_connected(&self) -> DeviceManagerResult {
        self.lease.mark_connected()
    }

    pub(super) fn into_registration(self) -> InterruptEndpointRegistration {
        InterruptEndpointRegistration { lease: self.lease }
    }
}

impl core::fmt::Debug for MsiClaim {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("MsiClaim")
            .field("resource", self.resource())
            .finish_non_exhaustive()
    }
}

/// A wired connection plus the registration capability that keeps its plan
/// claim alive.
pub struct PlannedIrqConnection {
    line: IrqLine,
    registration: InterruptEndpointRegistration,
}

impl PlannedIrqConnection {
    pub(super) const fn new(line: IrqLine, registration: InterruptEndpointRegistration) -> Self {
        Self { line, registration }
    }

    /// Separates the runtime line from its bundle registration capability.
    pub fn into_parts(self) -> (IrqLine, InterruptEndpointRegistration) {
        (self.line, self.registration)
    }
}

impl core::fmt::Debug for PlannedIrqConnection {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PlannedIrqConnection")
            .field("resource", self.registration.resource())
            .finish_non_exhaustive()
    }
}

/// An MSI connection plus the registration capability that keeps its plan
/// claim alive.
pub struct PlannedMsiConnection {
    endpoint: MsiEndpoint,
    registration: InterruptEndpointRegistration,
}

impl PlannedMsiConnection {
    pub(super) const fn new(
        endpoint: MsiEndpoint,
        registration: InterruptEndpointRegistration,
    ) -> Self {
        Self {
            endpoint,
            registration,
        }
    }

    /// Separates the runtime endpoint from its bundle registration capability.
    pub fn into_parts(self) -> (MsiEndpoint, InterruptEndpointRegistration) {
        (self.endpoint, self.registration)
    }
}

impl core::fmt::Debug for PlannedMsiConnection {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PlannedMsiConnection")
            .field("resource", self.registration.resource())
            .finish_non_exhaustive()
    }
}

/// RAII registration for one planner-authorized interrupt endpoint.
///
/// The constructor is private. It can only be obtained by connecting an opaque
/// claim issued for the same topology.
pub struct InterruptEndpointRegistration {
    lease: InterruptClaimLease,
}

impl InterruptEndpointRegistration {
    /// Returns the auditable endpoint resource derived from the machine plan.
    pub fn resource(&self) -> &Resource {
        self.lease.resource()
    }

    pub(crate) fn belongs_to(&self, topology: &InterruptTopology) -> bool {
        Arc::ptr_eq(&self.lease.domain, topology.claim_domain())
    }

    pub(crate) const fn key(&self) -> InterruptEndpointKey {
        self.lease.key
    }
}

impl core::fmt::Debug for InterruptEndpointRegistration {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InterruptEndpointRegistration")
            .field("resource", self.resource())
            .finish_non_exhaustive()
    }
}

pub(super) struct InterruptClaimDomain {
    state: SpinRaw<ClaimDomainState>,
}

impl InterruptClaimDomain {
    pub(super) const fn new() -> Self {
        Self {
            state: SpinRaw::new(ClaimDomainState {
                next_id: 0,
                claims: BTreeMap::new(),
            }),
        }
    }

    pub(super) fn claim_wired(
        self: &Arc<Self>,
        resource: Resource,
    ) -> DeviceManagerResult<WiredIrqClaim> {
        Ok(WiredIrqClaim {
            lease: self.reserve(resource)?,
        })
    }

    pub(super) fn claim_msi(self: &Arc<Self>, resource: Resource) -> DeviceManagerResult<MsiClaim> {
        Ok(MsiClaim {
            lease: self.reserve(resource)?,
        })
    }

    fn reserve(self: &Arc<Self>, resource: Resource) -> DeviceManagerResult<InterruptClaimLease> {
        let key =
            resource
                .interrupt_endpoint_key()
                .ok_or_else(|| DeviceManagerError::InvalidInput {
                    operation: "reserve planned interrupt endpoint",
                    detail: "claim resource is not an interrupt endpoint".into(),
                })?;
        let mut state = self.state.lock();
        state.validate_reservation(key, &resource)?;
        let id = state.next_id;
        state.next_id =
            state
                .next_id
                .checked_add(1)
                .ok_or_else(|| DeviceManagerError::ResourceConflict {
                    operation: "allocate interrupt claim identifier",
                    detail: "the interrupt claim identifier space is exhausted".into(),
                })?;
        state.claims.insert(
            id,
            ClaimRecord {
                key,
                resource: resource.clone(),
                connected: false,
            },
        );
        Ok(InterruptClaimLease {
            domain: self.clone(),
            id,
            key,
            resource,
        })
    }

    fn mark_connected(&self, id: u64, resource: &Resource) -> DeviceManagerResult {
        let mut state = self.state.lock();
        let claim = state
            .claims
            .get_mut(&id)
            .ok_or_else(|| DeviceManagerError::InvalidInput {
                operation: "connect planned interrupt endpoint",
                detail: format!("interrupt claim {id} is no longer active"),
            })?;
        if claim.resource != *resource {
            return Err(DeviceManagerError::InvalidInput {
                operation: "connect planned interrupt endpoint",
                detail: "interrupt claim resource does not match its reservation".into(),
            });
        }
        if claim.connected {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "connect planned interrupt endpoint",
                detail: format!("interrupt claim {id} was already consumed"),
            });
        }
        claim.connected = true;
        Ok(())
    }

    fn release(&self, id: u64) {
        self.state.lock().claims.remove(&id);
    }

    pub(super) fn resources(&self) -> alloc::vec::Vec<Resource> {
        self.state
            .lock()
            .claims
            .values()
            .map(|claim| claim.resource.clone())
            .collect()
    }
}

struct ClaimDomainState {
    next_id: u64,
    claims: BTreeMap<u64, ClaimRecord>,
}

impl ClaimDomainState {
    fn validate_reservation(
        &self,
        requested_key: InterruptEndpointKey,
        requested: &Resource,
    ) -> DeviceManagerResult {
        for claim in self.claims.values() {
            let existing = &claim.resource;
            if claim.key == requested_key && requested.interrupt_endpoint_conflicts_with(existing) {
                return Err(endpoint_conflict(requested, existing));
            }
        }
        Ok(())
    }
}

struct ClaimRecord {
    key: InterruptEndpointKey,
    resource: Resource,
    connected: bool,
}

struct InterruptClaimLease {
    domain: Arc<InterruptClaimDomain>,
    id: u64,
    key: InterruptEndpointKey,
    resource: Resource,
}

impl InterruptClaimLease {
    const fn resource(&self) -> &Resource {
        &self.resource
    }

    fn mark_connected(&self) -> DeviceManagerResult {
        self.domain.mark_connected(self.id, &self.resource)
    }
}

impl Drop for InterruptClaimLease {
    fn drop(&mut self) {
        self.domain.release(self.id);
    }
}

fn endpoint_conflict(requested: &Resource, existing: &Resource) -> DeviceManagerError {
    DeviceManagerError::ResourceConflict {
        operation: "reserve planned interrupt endpoint",
        detail: format!("{requested:?} conflicts with active claim {existing:?}"),
    }
}
