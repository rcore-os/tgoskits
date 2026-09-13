use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::fmt;

use ax_sync::SpinLock;
use axdevice_base::{
    DeviceError, DeviceId, DeviceResult, RoutedAdmissionEpoch as RoutedGrantAdmissionEpoch,
    RoutedBindingGeneration, RoutedDeviceGrant, RoutedGrantScope,
};

use super::{
    DEFAULT_DRAIN_ATTEMPTS, EndpointIrqTransitionPermit, PciCommandState, PciFunction,
    PendingIrqWithdrawal,
};
use crate::{DeviceManagerError, DeviceManagerResult};

struct AdmissionState {
    open: bool,
    leases: usize,
    permits: usize,
}

pub(super) struct EndpointAdmission {
    generation: EndpointBindingGeneration,
    epoch: RoutedAdmissionEpoch,
    state: SpinLock<AdmissionState>,
    #[cfg(test)]
    drain_observed_hook: SpinLock<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl EndpointAdmission {
    pub(super) fn new(generation: EndpointBindingGeneration, epoch: RoutedAdmissionEpoch) -> Self {
        Self {
            generation,
            epoch,
            state: SpinLock::new(AdmissionState {
                open: true,
                leases: 0,
                permits: 0,
            }),
            #[cfg(test)]
            drain_observed_hook: SpinLock::new(None),
        }
    }

    #[cfg(test)]
    pub(super) fn set_drain_observed_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.drain_observed_hook.lock_irqsave() = Some(hook);
    }

    #[cfg(test)]
    fn notify_drain_observed(&self) {
        let hook = self.drain_observed_hook.lock_irqsave().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    pub(super) fn acquire(
        self: &Arc<Self>,
        token: &EndpointRouteToken,
    ) -> DeviceResult<AdmissionLease> {
        let mut state = self.state.lock_irqsave();
        Self::validate_route(&state, self.generation, self.epoch, token)?;
        state.leases = state
            .leases
            .checked_add(1)
            .ok_or(DeviceError::InvalidState {
                operation: "admit PCI endpoint route",
                detail: "PCI endpoint route lease count is exhausted".into(),
            })?;
        Ok(AdmissionLease {
            admission: self.clone(),
        })
    }

    /// Validates and upgrades one routed callback while holding the binding
    /// admission gate. Once this returns successfully, closing the shared
    /// admission cannot revoke the callback before its scope is dropped.
    pub(super) fn acquire_scoped(
        self: &Arc<Self>,
        token: &EndpointRouteToken,
        dma_enabled: bool,
    ) -> DeviceResult<(AdmissionLease, RoutedDeviceGrant, RoutedGrantScope)> {
        let mut state = self.state.lock_irqsave();
        Self::validate_route(&state, self.generation, self.epoch, token)?;
        let (grant, grant_scope) =
            token
                .grant(dma_enabled)
                .admit()
                .ok_or(DeviceError::InvalidState {
                    operation: "admit PCI endpoint route",
                    detail: "PCI endpoint grant admission is closed or stale".into(),
                })?;
        state.leases = state
            .leases
            .checked_add(1)
            .ok_or(DeviceError::InvalidState {
                operation: "admit PCI endpoint route",
                detail: "PCI endpoint route lease count is exhausted".into(),
            })?;
        Ok((
            AdmissionLease {
                admission: self.clone(),
            },
            grant,
            grant_scope,
        ))
    }

    fn validate_route(
        state: &AdmissionState,
        generation: EndpointBindingGeneration,
        epoch: RoutedAdmissionEpoch,
        token: &EndpointRouteToken,
    ) -> DeviceResult {
        if !state.open || token.binding_generation != generation || token.admission_epoch != epoch {
            return Err(DeviceError::InvalidState {
                operation: "admit PCI endpoint route",
                detail: "PCI endpoint route admission is closed or stale".into(),
            });
        }
        Ok(())
    }

    pub(super) fn acquire_irq_permit(self: &Arc<Self>) -> DeviceResult<IrqPermitLease> {
        let mut state = self.state.lock_irqsave();
        if !state.open {
            return Err(DeviceError::InvalidState {
                operation: "publish PCI endpoint interrupt state",
                detail: "PCI endpoint route admission is closed".into(),
            });
        }
        state.permits = state
            .permits
            .checked_add(1)
            .ok_or(DeviceError::InvalidState {
                operation: "publish PCI endpoint interrupt state",
                detail: "PCI interrupt transition permit count is exhausted".into(),
            })?;
        Ok(IrqPermitLease {
            admission: self.clone(),
        })
    }

    #[cfg(test)]
    pub(super) fn close(&self) {
        self.state.lock_irqsave().open = false;
    }

    /// Closes new route admission and its ordinary grant under one gate.
    pub(super) fn close_with_grant(&self, grant: &RoutedDeviceGrant) {
        let mut state = self.state.lock_irqsave();
        state.open = false;
        grant.close_admission();
    }

    pub(super) fn wait_for_irq_permits(&self) -> DeviceManagerResult {
        self.wait_for_irq_permits_with_budget(DEFAULT_DRAIN_ATTEMPTS)
    }

    pub(super) fn wait_for_irq_permits_with_budget(&self, attempts: usize) -> DeviceManagerResult {
        for _ in 0..=attempts {
            if self.state.lock_irqsave().permits == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(DeviceManagerError::InvalidState {
            operation: "drain PCI endpoint IRQ permits",
            detail: "PCI endpoint IRQ permit drain exceeded its bounded wait budget".into(),
        })
    }

    pub(super) fn wait_for_idle(&self) -> DeviceManagerResult {
        self.wait_for_idle_with_budget(DEFAULT_DRAIN_ATTEMPTS)
    }

    fn wait_for_idle_with_budget(&self, attempts: usize) -> DeviceManagerResult {
        for _ in 0..=attempts {
            let idle = {
                let state = self.state.lock_irqsave();
                state.leases == 0 && state.permits == 0
            };
            if idle {
                return Ok(());
            }
            #[cfg(test)]
            self.notify_drain_observed();
            core::hint::spin_loop();
        }
        Err(DeviceManagerError::InvalidState {
            operation: "drain PCI endpoint route leases",
            detail: "PCI endpoint route drain exceeded its bounded wait budget".into(),
        })
    }

    /// Reopens a fresh route admission and publishes the matching grant epoch
    /// under one gate.
    pub(super) fn open_with_grant(
        &self,
        grant: &RoutedDeviceGrant,
        epoch: RoutedGrantAdmissionEpoch,
    ) {
        let mut state = self.state.lock_irqsave();
        state.open = true;
        grant.reopen_admission(epoch);
    }
}

pub(super) struct AdmissionLease {
    admission: Arc<EndpointAdmission>,
}

impl Drop for AdmissionLease {
    fn drop(&mut self) {
        self.admission.state.lock_irqsave().leases -= 1;
    }
}

pub(super) struct IrqPermitLease {
    admission: Arc<EndpointAdmission>,
}

impl Drop for IrqPermitLease {
    fn drop(&mut self) {
        self.admission.state.lock_irqsave().permits -= 1;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EndpointBindingGeneration(pub(super) u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RoutedAdmissionEpoch(pub(super) u64);

/// Non-capability token identifying one active endpoint binding generation and
/// routed admission epoch.
#[derive(Clone)]
pub struct EndpointRouteToken {
    pub(super) device: DeviceId,
    pub(super) binding_generation: EndpointBindingGeneration,
    pub(super) admission_epoch: RoutedAdmissionEpoch,
    pub(super) admission: Arc<EndpointAdmission>,
    pub(super) grant: RoutedDeviceGrant,
}

impl EndpointRouteToken {
    /// Returns the final device selected by this route.
    pub const fn device_id(&self) -> DeviceId {
        self.device
    }

    /// Returns the binding generation selected by this route.
    pub const fn binding_generation(&self) -> u64 {
        self.binding_generation.0
    }

    /// Returns the admission epoch selected by this route.
    pub const fn admission_epoch(&self) -> u64 {
        self.admission_epoch.0
    }

    pub(super) fn grant(&self, dma_enabled: bool) -> RoutedDeviceGrant {
        self.grant.with_dma_enabled(dma_enabled)
    }

    pub(crate) fn snapshot_if_admitted(&self) -> Option<Self> {
        let state = self.admission.state.lock_irqsave();
        state.open.then(|| self.clone())
    }
}

impl PartialEq for EndpointRouteToken {
    fn eq(&self, other: &Self) -> bool {
        self.device == other.device
            && self.binding_generation == other.binding_generation
            && self.admission_epoch == other.admission_epoch
            && Arc::ptr_eq(&self.admission, &other.admission)
    }
}

impl Eq for EndpointRouteToken {}

impl fmt::Debug for EndpointRouteToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EndpointRouteToken")
            .field("device", &self.device)
            .field("binding_generation", &self.binding_generation())
            .field("admission_epoch", &self.admission_epoch())
            .finish_non_exhaustive()
    }
}

pub(super) struct RoutedEndpointLease {
    pub(super) endpoint: Arc<dyn PciFunction>,
    pub(super) _admission: Arc<EndpointAdmission>,
    pub(super) _lease: AdmissionLease,
    pub(super) grant: RoutedDeviceGrant,
    // The scoped grant remains admitted until this lease is dropped.
    pub(super) _grant_scope: RoutedGrantScope,
}

pub(super) struct RoutedEndpoint {
    pub(super) token: EndpointRouteToken,
    pub(super) function: Arc<dyn PciFunction>,
}

#[derive(Default)]
pub(super) struct EndpointRouterState {
    pub(super) next_generation: u64,
    pub(super) endpoints: BTreeMap<DeviceId, RoutedEndpoint>,
}

pub(super) struct EndpointRouter {
    pub(super) state: SpinLock<EndpointRouterState>,
    #[cfg(test)]
    reset_admission_hook: SpinLock<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl EndpointRouter {
    pub(super) fn new() -> Self {
        Self {
            state: SpinLock::new(EndpointRouterState::default()),
            #[cfg(test)]
            reset_admission_hook: SpinLock::new(None),
        }
    }

    #[cfg(test)]
    pub(super) fn set_reset_admission_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.reset_admission_hook.lock_irqsave() = Some(hook);
    }

    #[cfg(test)]
    fn notify_reset_admission(&self) {
        let hook = self.reset_admission_hook.lock_irqsave().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    pub(super) fn activate(
        &self,
        device: DeviceId,
        function: Arc<dyn PciFunction>,
    ) -> DeviceManagerResult<EndpointRouteToken> {
        let mut state = self.state.lock_irqsave();
        if state.endpoints.contains_key(&device) {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "bind PCI endpoint route",
                detail: alloc::format!(
                    "device {} already has an active PCI route",
                    device.as_u32()
                ),
            });
        }
        state.next_generation = state.next_generation.checked_add(1).ok_or_else(|| {
            DeviceManagerError::InvalidState {
                operation: "bind PCI endpoint route",
                detail: "PCI binding generation is exhausted".into(),
            }
        })?;
        let generation = EndpointBindingGeneration(state.next_generation);
        let epoch = RoutedAdmissionEpoch(1);
        let admission = Arc::new(EndpointAdmission::new(generation, epoch));
        let grant = RoutedDeviceGrant::new(
            device,
            RoutedBindingGeneration::new(state.next_generation),
            RoutedGrantAdmissionEpoch::new(1),
            false,
        );
        let token = EndpointRouteToken {
            device,
            binding_generation: generation,
            admission_epoch: epoch,
            admission: admission.clone(),
            grant,
        };
        state.endpoints.insert(
            device,
            RoutedEndpoint {
                token: token.clone(),
                function,
            },
        );
        Ok(token)
    }

    pub(super) fn invalidate(&self, token: &EndpointRouteToken) -> Option<Arc<dyn PciFunction>> {
        let mut state = self.state.lock_irqsave();
        if state
            .endpoints
            .get(&token.device)
            .is_some_and(|entry| entry.token == *token)
        {
            let entry = state.endpoints.remove(&token.device)?;
            entry.token.admission.close_with_grant(&entry.token.grant);
            return Some(entry.function);
        }
        None
    }

    pub(super) fn invalidate_device(
        &self,
        device: DeviceId,
    ) -> Option<(Arc<dyn PciFunction>, Arc<EndpointAdmission>)> {
        let mut state = self.state.lock_irqsave();
        let entry = state.endpoints.remove(&device)?;
        entry.token.admission.close_with_grant(&entry.token.grant);
        Some((entry.function, entry.token.admission))
    }

    pub(super) fn endpoint(
        &self,
        token: &EndpointRouteToken,
    ) -> DeviceResult<Arc<dyn PciFunction>> {
        let state = self.state.lock_irqsave();
        state
            .endpoints
            .get(&token.device)
            .filter(|entry| entry.token == *token)
            .map(|entry| entry.function.clone())
            .ok_or_else(|| DeviceError::InvalidState {
                operation: "dispatch PCI endpoint route",
                detail: "PCI endpoint route token is stale".into(),
            })
    }

    pub(super) fn lease(
        &self,
        token: &EndpointRouteToken,
        dma_enabled: bool,
    ) -> DeviceResult<RoutedEndpointLease> {
        let endpoint = self.endpoint(token)?;
        let (lease, grant, grant_scope) =
            token.admission.clone().acquire_scoped(token, dma_enabled)?;
        Ok(RoutedEndpointLease {
            endpoint,
            _admission: token.admission.clone(),
            _lease: lease,
            grant,
            _grant_scope: grant_scope,
        })
    }

    pub(super) fn reset_endpoints(
        &self,
        commands: &[(DeviceId, PciCommandState)],
    ) -> DeviceManagerResult {
        let endpoints = {
            let state = self.state.lock_irqsave();
            if commands.len() != state.endpoints.len() {
                return Err(DeviceManagerError::InvalidState {
                    operation: "reset PCI endpoints",
                    detail: "PCI root and endpoint route sets are inconsistent".into(),
                });
            }
            commands
                .iter()
                .filter_map(|(device, command)| {
                    state
                        .endpoints
                        .get(device)
                        .map(|endpoint| (endpoint.function.clone(), *command))
                })
                .collect::<Vec<_>>()
        };
        let mut first_error = None;
        for (endpoint, command) in endpoints {
            if let Err(error) = endpoint.reset(command).map_err(DeviceManagerError::Device)
                && first_error.is_none()
            {
                first_error = Some(error);
            }

            // The fresh route admission is intentionally still closed here.
            // The lifecycle owner has drained the old admission, so this
            // owner-side transition is authorized directly by the reset
            // phase's permit rather than by a routed callback permit.
            let mut permit = EndpointIrqTransitionPermit { _private: () };
            if let Err(error) = endpoint
                .withdraw_irq(&mut permit)
                .map_err(DeviceManagerError::Device)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) fn reset_admissions(
        &self,
    ) -> DeviceManagerResult<Vec<(EndpointRouteToken, EndpointRouteToken)>> {
        let (replacements, old_admissions) = {
            let mut state = self.state.lock_irqsave();
            for endpoint in state.endpoints.values() {
                if endpoint.token.admission_epoch() == u64::MAX {
                    return Err(DeviceManagerError::InvalidState {
                        operation: "reset PCI endpoint route admission",
                        detail: "PCI route admission epoch is exhausted".into(),
                    });
                }
            }
            let mut replacements = Vec::with_capacity(state.endpoints.len());
            let mut old_admissions = Vec::with_capacity(state.endpoints.len());
            for endpoint in state.endpoints.values_mut() {
                let old = endpoint.token.clone();
                let epoch = old.admission_epoch() + 1;
                old.admission.close_with_grant(&old.grant);
                old_admissions.push(old.admission.clone());
                let admission = Arc::new(EndpointAdmission::new(
                    old.binding_generation,
                    RoutedAdmissionEpoch(epoch),
                ));
                let grant = old
                    .grant
                    .with_admission_epoch(RoutedGrantAdmissionEpoch::new(epoch));
                admission.close_with_grant(&grant);
                let token = EndpointRouteToken {
                    device: old.device,
                    binding_generation: old.binding_generation,
                    admission_epoch: RoutedAdmissionEpoch(epoch),
                    admission,
                    grant,
                };
                endpoint.token = token.clone();
                replacements.push((old, token));
            }
            (replacements, old_admissions)
        };
        #[cfg(test)]
        self.notify_reset_admission();
        for admission in old_admissions {
            admission.wait_for_idle()?;
        }
        Ok(replacements)
    }

    pub(super) fn close_admissions_and_drain(&self) -> DeviceManagerResult {
        let admissions = {
            let state = self.state.lock_irqsave();
            for endpoint in state.endpoints.values() {
                endpoint
                    .token
                    .admission
                    .close_with_grant(&endpoint.token.grant);
            }
            state
                .endpoints
                .values()
                .map(|endpoint| endpoint.token.admission.clone())
                .collect::<Vec<_>>()
        };
        for admission in admissions {
            admission.wait_for_idle()?;
        }
        Ok(())
    }

    pub(super) fn invalidate_all(&self) -> (Vec<PendingIrqWithdrawal>, DeviceManagerResult) {
        let pending = {
            let mut state = self.state.lock_irqsave();
            let pending = state
                .endpoints
                .values()
                .map(|endpoint| {
                    endpoint
                        .token
                        .admission
                        .close_with_grant(&endpoint.token.grant);
                    PendingIrqWithdrawal {
                        device: endpoint.token.device_id(),
                        function: endpoint.function.clone(),
                        admission: endpoint.token.admission.clone(),
                    }
                })
                .collect::<Vec<_>>();
            state.endpoints.clear();
            pending
        };
        let mut first_error = None;
        for withdrawal in &pending {
            if let Err(error) = withdrawal.admission.wait_for_irq_permits()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        (pending, first_error.map_or(Ok(()), Err))
    }

    pub(super) fn open_admissions(&self) {
        let state = self.state.lock_irqsave();
        for endpoint in state.endpoints.values() {
            endpoint.token.admission.open_with_grant(
                &endpoint.token.grant,
                RoutedGrantAdmissionEpoch::new(endpoint.token.admission_epoch()),
            );
        }
    }
}
