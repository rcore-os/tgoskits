use alloc::{collections::BTreeMap, sync::Arc};

use ax_kspin::SpinNoIrq;
use axdevice_base::InterruptTriggerMode;

use super::{
    ArmSpiIntId, ArmSpiRoute, DeliveryDescriptor, DeliveryEpoch, DeliveryError, DeliveryOutcome,
    FoldOutcome, ReconcileError, ReconcileOutcome, ResidentLrState, ResidentObservation,
    ResidentUpdate, ServiceHint, TargetSummary, VgicVcpuId,
};
use crate::{VgicError, VgicResult};

/// Durable, IRQ-safe state of all module-owned virtual SPIs in one VM.
pub struct ArmVgicController {
    state: SpinNoIrq<ControllerState>,
}

impl ArmVgicController {
    /// Creates and seals a controller from fixed routes.
    pub fn new(
        routes: impl IntoIterator<Item = (ArmSpiRoute, InterruptTriggerMode)>,
    ) -> VgicResult<Self> {
        let mut records = BTreeMap::new();
        for (route, trigger) in routes {
            insert_route(&mut records, route, trigger)?;
        }
        Ok(Self {
            state: SpinNoIrq::new(ControllerState::sealed(records)),
        })
    }

    /// Records one edge pulse.
    pub fn pulse(&self, intid: ArmSpiIntId) -> VgicResult<ServiceHint> {
        let mut state = self.ready_state()?;
        let record = state.record_mut(intid)?;
        record.ensure_trigger(InterruptTriggerMode::EdgeTriggered)?;
        let was_pending = record.pending_latch;
        record.pending_latch = true;
        Ok(if was_pending {
            ServiceHint::None
        } else {
            record.service_hint()
        })
    }

    /// Changes the asserted state of one level-triggered input.
    pub fn set_level(&self, intid: ArmSpiIntId, asserted: bool) -> VgicResult<ServiceHint> {
        let mut state = self.ready_state()?;
        let record = state.record_mut(intid)?;
        record.ensure_trigger(InterruptTriggerMode::LevelTriggered)?;
        if record.line_asserted == asserted {
            return Ok(ServiceHint::None);
        }
        record.line_asserted = asserted;
        Ok(record.service_hint())
    }

    /// Changes the guest-visible enable state of one registered SPI.
    pub fn set_enabled(&self, intid: ArmSpiIntId, enabled: bool) -> VgicResult<ServiceHint> {
        let mut state = self.ready_state()?;
        let record = state.record_mut(intid)?;
        if record.enabled == enabled {
            return Ok(ServiceHint::None);
        }
        record.enabled = enabled;
        Ok(record.service_hint())
    }

    /// Returns whether a registered SPI is enabled; unregistered INTIDs are RAZ.
    pub fn is_enabled(&self, intid: ArmSpiIntId) -> VgicResult<bool> {
        let state = self.ready_state()?;
        Ok(state
            .records
            .get(&intid)
            .is_some_and(|record| record.enabled))
    }

    /// Atomically installs one deliverable SPI and commits its epoch.
    ///
    /// The installer runs under the controller's short IRQ-safe lock and must
    /// only write one already-selected CPU-local LR. It must not sleep,
    /// allocate, send an IPI, or call a device/runtime callback.
    pub fn deliver_one<E>(
        &self,
        target: VgicVcpuId,
        install: impl FnOnce(DeliveryDescriptor) -> Result<(), E>,
    ) -> Result<DeliveryOutcome, DeliveryError<E>> {
        let mut state = self.ready_state().map_err(DeliveryError::Controller)?;
        let Some(intid) = state.records.iter().find_map(|(intid, record)| {
            (record.target == target && record.deliverable()).then_some(*intid)
        }) else {
            return Ok(DeliveryOutcome::NoWork);
        };
        let epoch = state.current_epoch().map_err(DeliveryError::Controller)?;
        let trigger = state
            .records
            .get(&intid)
            .expect("selected record must exist")
            .trigger;
        let descriptor = DeliveryDescriptor::new(intid, epoch, trigger);
        install(descriptor).map_err(DeliveryError::Installer)?;
        state.commit_epoch();
        let record = state
            .records
            .get_mut(&intid)
            .expect("selected record must exist");
        record.consume_source_pending();
        record.inflight = Some(ResidentInstance {
            owner: target,
            epoch,
        });
        Ok(DeliveryOutcome::Installed { intid, epoch })
    }

    /// Folds a local LR observation into durable state.
    pub fn fold(&self, observation: ResidentObservation) -> VgicResult<FoldOutcome> {
        let mut state = self.ready_state()?;
        let record = state.record_mut(observation.intid())?;
        record.ensure_observation(observation)?;
        Ok(record.fold(observation.state()))
    }

    /// Reconciles durable source state with one mapped local LR.
    ///
    /// `apply` has the same local-register-only restrictions as the installer
    /// passed to [`deliver_one`](Self::deliver_one).
    pub fn reconcile<E>(
        &self,
        observation: ResidentObservation,
        apply: impl FnOnce(ResidentUpdate) -> Result<(), E>,
    ) -> Result<ReconcileOutcome, ReconcileError<E>> {
        let mut state = self.ready_state().map_err(ReconcileError::Controller)?;
        let record = state
            .record_mut(observation.intid())
            .map_err(ReconcileError::Controller)?;
        record
            .ensure_observation(observation)
            .map_err(ReconcileError::Controller)?;
        record.reconcile(observation.state(), apply)
    }

    /// Requests architectural deactivation of the current active instance.
    pub fn request_deactivation(&self, intid: ArmSpiIntId) -> VgicResult<ServiceHint> {
        let mut state = self.ready_state()?;
        let record = state.record_mut(intid)?;
        let Some(instance) = record.inflight else {
            return Ok(ServiceHint::None);
        };
        if !record.active {
            return Ok(ServiceHint::None);
        }
        record.deactivation = Some(instance);
        Ok(ServiceHint::Target(instance.owner))
    }

    /// Returns target-local work without exposing individual records.
    pub fn target_summary(&self, target: VgicVcpuId) -> VgicResult<TargetSummary> {
        let state = self.ready_state()?;
        let mut deliverable = false;
        let mut resident = false;
        for record in state
            .records
            .values()
            .filter(|record| record.target == target)
        {
            deliverable |= record.deliverable();
            resident |= record.resident_needs_service();
        }
        Ok(TargetSummary::new(deliverable, resident))
    }

    fn ready_state(&self) -> VgicResult<ax_kspin::SpinNoIrqGuard<'_, ControllerState>> {
        let state = self.state.lock();
        if state.sealed {
            Ok(state)
        } else {
            Err(VgicError::NotReady)
        }
    }
}

/// Control-plane route builder. Runtime operations fail until it is finished.
pub struct ArmVgicControllerBuilder {
    controller: Arc<ArmVgicController>,
}

impl ArmVgicControllerBuilder {
    /// Creates an unsealed controller builder.
    pub fn new() -> Self {
        Self {
            controller: Arc::new(ArmVgicController {
                state: SpinNoIrq::new(ControllerState::unsealed()),
            }),
        }
    }

    /// Returns the controller identity needed while assembling VM resources.
    pub fn controller(&self) -> Arc<ArmVgicController> {
        self.controller.clone()
    }

    /// Registers one fixed route before sealing.
    pub fn register(&mut self, route: ArmSpiRoute, trigger: InterruptTriggerMode) -> VgicResult {
        let mut state = self.controller.state.lock();
        if state.sealed {
            return Err(VgicError::BadState {
                operation: "register SPI route",
                detail: alloc::string::String::from("controller is already sealed"),
            });
        }
        insert_route(&mut state.records, route, trigger)
    }

    /// Seals the route table and publishes it to runtime operations.
    pub fn finish(self) -> VgicResult<Arc<ArmVgicController>> {
        self.controller.state.lock().sealed = true;
        Ok(self.controller)
    }
}

impl Default for ArmVgicControllerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

struct ControllerState {
    records: BTreeMap<ArmSpiIntId, InterruptRecord>,
    next_epoch: u64,
    sealed: bool,
}

impl ControllerState {
    fn unsealed() -> Self {
        Self {
            records: BTreeMap::new(),
            next_epoch: 1,
            sealed: false,
        }
    }

    fn sealed(records: BTreeMap<ArmSpiIntId, InterruptRecord>) -> Self {
        Self {
            records,
            next_epoch: 1,
            sealed: true,
        }
    }

    fn record_mut(&mut self, intid: ArmSpiIntId) -> VgicResult<&mut InterruptRecord> {
        self.records
            .get_mut(&intid)
            .ok_or(VgicError::UnregisteredSpi {
                intid: intid.as_u32(),
            })
    }

    fn current_epoch(&self) -> VgicResult<DeliveryEpoch> {
        if self.next_epoch == 0 {
            return Err(VgicError::DeliveryEpochExhausted);
        }
        Ok(DeliveryEpoch::new(self.next_epoch))
    }

    fn commit_epoch(&mut self) {
        self.next_epoch = self.next_epoch.checked_add(1).unwrap_or(0);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResidentInstance {
    owner: VgicVcpuId,
    epoch: DeliveryEpoch,
}

struct InterruptRecord {
    intid: ArmSpiIntId,
    target: VgicVcpuId,
    trigger: InterruptTriggerMode,
    enabled: bool,
    line_asserted: bool,
    pending_latch: bool,
    active: bool,
    inflight: Option<ResidentInstance>,
    deactivation: Option<ResidentInstance>,
}

impl InterruptRecord {
    fn new(route: ArmSpiRoute, trigger: InterruptTriggerMode) -> Self {
        Self {
            intid: route.intid(),
            target: route.target(),
            trigger,
            enabled: true,
            line_asserted: false,
            pending_latch: false,
            active: false,
            inflight: None,
            deactivation: None,
        }
    }

    fn ensure_trigger(&self, expected: InterruptTriggerMode) -> VgicResult {
        if self.trigger == expected {
            Ok(())
        } else {
            Err(VgicError::TriggerMismatch {
                intid: self.intid.as_u32(),
                expected,
                actual: self.trigger,
            })
        }
    }

    fn source_pending(&self) -> bool {
        self.pending_latch
            || (self.trigger == InterruptTriggerMode::LevelTriggered && self.line_asserted)
    }

    fn deliverable(&self) -> bool {
        self.enabled
            && !self.active
            && self.inflight.is_none()
            && self.deactivation.is_none()
            && self.source_pending()
    }

    fn consume_source_pending(&mut self) {
        self.pending_latch = false;
    }

    fn service_hint(&self) -> ServiceHint {
        if self.deliverable() || self.resident_needs_service() {
            ServiceHint::Target(self.inflight.map_or(self.target, |instance| instance.owner))
        } else {
            ServiceHint::None
        }
    }

    fn resident_needs_service(&self) -> bool {
        self.inflight.is_some()
            && (self.deactivation.is_some()
                || self.pending_latch
                || (!self.enabled && !self.active)
                || (self.trigger == InterruptTriggerMode::LevelTriggered
                    && !self.line_asserted
                    && !self.active))
    }

    fn ensure_observation(&self, observation: ResidentObservation) -> VgicResult {
        let expected = ResidentInstance {
            owner: observation.target(),
            epoch: observation.epoch(),
        };
        if self.inflight == Some(expected) {
            Ok(())
        } else {
            Err(VgicError::ResidentMismatch {
                intid: observation.intid().as_u32(),
            })
        }
    }

    fn fold(&mut self, observed: ResidentLrState) -> FoldOutcome {
        match observed {
            ResidentLrState::Invalid => {
                self.active = false;
                self.inflight = None;
                self.deactivation = None;
                FoldOutcome::Released
            }
            ResidentLrState::Pending => {
                self.active = false;
                self.pending_latch = false;
                FoldOutcome::Resident
            }
            ResidentLrState::Active => {
                self.active = true;
                FoldOutcome::Resident
            }
            ResidentLrState::ActivePending => {
                self.active = true;
                self.pending_latch = false;
                FoldOutcome::Resident
            }
        }
    }

    fn reconcile<E>(
        &mut self,
        observed: ResidentLrState,
        apply: impl FnOnce(ResidentUpdate) -> Result<(), E>,
    ) -> Result<ReconcileOutcome, ReconcileError<E>> {
        let update = self.planned_update(observed);
        let Some(update) = update else {
            if matches!(
                observed,
                ResidentLrState::Pending | ResidentLrState::ActivePending
            ) {
                self.pending_latch = false;
            }
            if self.deactivation == self.inflight && observed == ResidentLrState::Pending {
                self.active = false;
                self.deactivation = None;
            }
            return Ok(ReconcileOutcome::Resident);
        };
        apply(update).map_err(ReconcileError::Apply)?;
        self.commit_update(update);
        Ok(match update {
            ResidentUpdate::Invalidate => ReconcileOutcome::Released,
            ResidentUpdate::SetState(_) => ReconcileOutcome::Updated,
        })
    }

    fn planned_update(&self, observed: ResidentLrState) -> Option<ResidentUpdate> {
        if self.deactivation == self.inflight {
            return match observed {
                ResidentLrState::Active => Some(ResidentUpdate::Invalidate),
                ResidentLrState::ActivePending => {
                    Some(ResidentUpdate::SetState(ResidentLrState::Pending))
                }
                ResidentLrState::Invalid | ResidentLrState::Pending => None,
            };
        }
        match observed {
            ResidentLrState::Pending
                if !self.enabled
                    || (self.trigger == InterruptTriggerMode::LevelTriggered
                        && !self.line_asserted) =>
            {
                Some(ResidentUpdate::Invalidate)
            }
            ResidentLrState::Active
                if self.trigger == InterruptTriggerMode::EdgeTriggered && self.pending_latch =>
            {
                Some(ResidentUpdate::SetState(ResidentLrState::ActivePending))
            }
            _ => None,
        }
    }

    fn commit_update(&mut self, update: ResidentUpdate) {
        match update {
            ResidentUpdate::Invalidate => {
                self.active = false;
                self.inflight = None;
                self.deactivation = None;
            }
            ResidentUpdate::SetState(ResidentLrState::Pending) => {
                self.active = false;
                self.pending_latch = false;
                self.deactivation = None;
            }
            ResidentUpdate::SetState(ResidentLrState::ActivePending) => {
                self.active = true;
                self.pending_latch = false;
                self.deactivation = None;
            }
            ResidentUpdate::SetState(ResidentLrState::Active) => {
                self.active = true;
                self.deactivation = None;
            }
            ResidentUpdate::SetState(ResidentLrState::Invalid) => {
                self.active = false;
                self.inflight = None;
                self.deactivation = None;
            }
        }
    }
}

fn insert_route(
    records: &mut BTreeMap<ArmSpiIntId, InterruptRecord>,
    route: ArmSpiRoute,
    trigger: InterruptTriggerMode,
) -> VgicResult {
    if records.contains_key(&route.intid()) {
        return Err(VgicError::DuplicateSpiRoute {
            intid: route.intid().as_u32(),
        });
    }
    records.insert(route.intid(), InterruptRecord::new(route, trigger));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intid(value: u32) -> ArmSpiIntId {
        ArmSpiIntId::new(value).unwrap()
    }
    fn target(value: u32) -> VgicVcpuId {
        VgicVcpuId::new(value)
    }
    fn controller(trigger: InterruptTriggerMode) -> ArmVgicController {
        ArmVgicController::new([(ArmSpiRoute::new(intid(32), target(0)), trigger)]).unwrap()
    }
    fn deliver(controller: &ArmVgicController) -> DeliveryDescriptor {
        let mut installed = None;
        controller
            .deliver_one(target(0), |descriptor| {
                installed = Some(descriptor);
                Ok::<_, ()>(())
            })
            .unwrap();
        installed.unwrap()
    }
    fn observe(descriptor: DeliveryDescriptor, state: ResidentLrState) -> ResidentObservation {
        ResidentObservation::new(
            target(0),
            descriptor.intid(),
            descriptor.epoch(),
            state,
            false,
        )
    }

    #[test]
    fn validates_intid_boundaries_and_builder_phase() {
        assert!(ArmSpiIntId::new(31).is_err());
        assert!(ArmSpiIntId::new(32).is_ok());
        assert!(ArmSpiIntId::new(512).is_ok());
        assert!(ArmSpiIntId::new(1019).is_ok());
        assert!(ArmSpiIntId::new(1020).is_err());
        let mut builder = ArmVgicControllerBuilder::new();
        let controller = builder.controller();
        assert_eq!(controller.pulse(intid(32)), Err(VgicError::NotReady));
        builder
            .register(
                ArmSpiRoute::new(intid(32), target(0)),
                InterruptTriggerMode::EdgeTriggered,
            )
            .unwrap();
        assert!(matches!(
            builder.register(
                ArmSpiRoute::new(intid(32), target(1)),
                InterruptTriggerMode::EdgeTriggered
            ),
            Err(VgicError::DuplicateSpiRoute { .. })
        ));
        builder.finish().unwrap().pulse(intid(32)).unwrap();
    }

    #[test]
    fn rejects_unregistered_sources_and_trigger_mismatches() {
        let controller = controller(InterruptTriggerMode::EdgeTriggered);
        assert!(matches!(
            controller.pulse(intid(33)),
            Err(VgicError::UnregisteredSpi { intid: 33 })
        ));
        assert!(matches!(
            controller.set_level(intid(32), true),
            Err(VgicError::TriggerMismatch { intid: 32, .. })
        ));
    }

    #[test]
    fn failed_install_does_not_consume_pending_or_epoch() {
        let controller = controller(InterruptTriggerMode::EdgeTriggered);
        controller.pulse(intid(32)).unwrap();
        assert_eq!(
            controller.deliver_one(target(0), |_| Err::<(), _>(7)),
            Err(DeliveryError::Installer(7))
        );
        let descriptor = deliver(&controller);
        assert_eq!(descriptor.epoch().as_u64(), 1);
    }

    #[test]
    fn edge_while_active_becomes_active_pending_without_replacing_epoch() {
        let controller = controller(InterruptTriggerMode::EdgeTriggered);
        controller.pulse(intid(32)).unwrap();
        let descriptor = deliver(&controller);
        controller
            .fold(observe(descriptor, ResidentLrState::Active))
            .unwrap();
        assert_eq!(
            controller.pulse(intid(32)).unwrap(),
            ServiceHint::Target(target(0))
        );
        let mut update = None;
        assert_eq!(
            controller
                .reconcile(observe(descriptor, ResidentLrState::Active), |value| {
                    update = Some(value);
                    Ok::<_, ()>(())
                })
                .unwrap(),
            ReconcileOutcome::Updated
        );
        assert_eq!(
            update,
            Some(ResidentUpdate::SetState(ResidentLrState::ActivePending))
        );
        assert_eq!(
            controller.deliver_one(target(0), |_| Ok::<_, ()>(())),
            Ok(DeliveryOutcome::NoWork)
        );
    }

    #[test]
    fn failed_reconcile_does_not_commit_the_planned_lr_update() {
        let controller = controller(InterruptTriggerMode::EdgeTriggered);
        controller.pulse(intid(32)).unwrap();
        let descriptor = deliver(&controller);
        controller
            .fold(observe(descriptor, ResidentLrState::Active))
            .unwrap();
        controller.pulse(intid(32)).unwrap();

        assert_eq!(
            controller.reconcile(observe(descriptor, ResidentLrState::Active), |_| Err(7)),
            Err(ReconcileError::Apply(7))
        );
        let mut retry = None;
        controller
            .reconcile(observe(descriptor, ResidentLrState::Active), |update| {
                retry = Some(update);
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(
            retry,
            Some(ResidentUpdate::SetState(ResidentLrState::ActivePending))
        );
    }

    #[test]
    fn deactivating_active_pending_leaves_one_pending_instance() {
        let controller = controller(InterruptTriggerMode::EdgeTriggered);
        controller.pulse(intid(32)).unwrap();
        let descriptor = deliver(&controller);
        controller
            .fold(observe(descriptor, ResidentLrState::Active))
            .unwrap();
        controller.pulse(intid(32)).unwrap();
        controller
            .reconcile(observe(descriptor, ResidentLrState::Active), |_| {
                Ok::<_, ()>(())
            })
            .unwrap();

        assert_eq!(
            controller.request_deactivation(intid(32)).unwrap(),
            ServiceHint::Target(target(0))
        );
        assert_eq!(
            controller
                .reconcile(
                    observe(descriptor, ResidentLrState::ActivePending),
                    |_| Ok::<_, ()>(())
                )
                .unwrap(),
            ReconcileOutcome::Updated
        );
        assert_eq!(
            controller.request_deactivation(intid(32)).unwrap(),
            ServiceHint::None
        );
        assert_eq!(
            controller
                .reconcile(
                    observe(descriptor, ResidentLrState::Pending),
                    |_| -> Result<(), ()> {
                        panic!("a completed command must not rewrite a pending LR")
                    }
                )
                .unwrap(),
            ReconcileOutcome::Resident
        );
    }

    #[test]
    fn level_lower_revokes_pending_but_not_active() {
        let controller = controller(InterruptTriggerMode::LevelTriggered);
        controller.set_level(intid(32), true).unwrap();
        let first = deliver(&controller);
        controller.set_level(intid(32), false).unwrap();
        assert_eq!(
            controller
                .reconcile(
                    observe(first, ResidentLrState::Pending),
                    |_| Ok::<_, ()>(())
                )
                .unwrap(),
            ReconcileOutcome::Released
        );

        controller.set_level(intid(32), true).unwrap();
        let second = deliver(&controller);
        controller
            .fold(observe(second, ResidentLrState::Active))
            .unwrap();
        controller.set_level(intid(32), false).unwrap();
        assert_eq!(
            controller
                .reconcile(
                    observe(second, ResidentLrState::Active),
                    |_| -> Result<(), ()> { panic!("active LR must stay resident") }
                )
                .unwrap(),
            ReconcileOutcome::Resident
        );
    }

    #[test]
    fn asserted_level_requeues_after_deactivation() {
        let controller = controller(InterruptTriggerMode::LevelTriggered);
        controller.set_level(intid(32), true).unwrap();
        let first = deliver(&controller);
        controller
            .fold(observe(first, ResidentLrState::Active))
            .unwrap();
        assert_eq!(
            controller.request_deactivation(intid(32)).unwrap(),
            ServiceHint::Target(target(0))
        );
        assert_eq!(
            controller
                .reconcile(observe(first, ResidentLrState::Active), |_| Ok::<_, ()>(()))
                .unwrap(),
            ReconcileOutcome::Released
        );
        let second = deliver(&controller);
        assert!(second.epoch() > first.epoch());
    }

    #[test]
    fn stale_observation_cannot_release_new_delivery() {
        let controller = controller(InterruptTriggerMode::EdgeTriggered);
        controller.pulse(intid(32)).unwrap();
        let first = deliver(&controller);
        controller
            .fold(observe(first, ResidentLrState::Invalid))
            .unwrap();
        controller.pulse(intid(32)).unwrap();
        let second = deliver(&controller);
        assert!(matches!(
            controller.fold(observe(first, ResidentLrState::Invalid)),
            Err(VgicError::ResidentMismatch { .. })
        ));
        assert_eq!(
            controller
                .fold(observe(second, ResidentLrState::Pending))
                .unwrap(),
            FoldOutcome::Resident
        );
    }

    #[test]
    fn enabling_a_pending_source_returns_target_hint() {
        let controller = controller(InterruptTriggerMode::EdgeTriggered);
        controller.set_enabled(intid(32), false).unwrap();
        assert_eq!(controller.pulse(intid(32)).unwrap(), ServiceHint::None);
        assert_eq!(
            controller.set_enabled(intid(32), true).unwrap(),
            ServiceHint::Target(target(0))
        );
    }
}
