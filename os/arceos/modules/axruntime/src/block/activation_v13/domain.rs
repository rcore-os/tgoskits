//! Final-owner installation and evidence service for one I/O domain.

use alloc::{boxed::Box, format, sync::Arc, vec::Vec};
use core::{
    mem,
    num::{NonZeroU64, NonZeroUsize},
};

use rdif_block::{
    BoundDomainProof, ControllerEpoch, ControllerFault, DomainOwnerBinding, EvidenceServiceResult,
    InstalledIoDomain, IoDomainIrqSource, QuiesceIntent, RecoveryEvidenceRetirePermit,
    RecoveryEvidenceRetired, UnboundIoDomain,
};

use super::{
    BoundEvidenceSource, ClosedSourceDisposition, FixedOwnershipTopology, QuiescedEvidenceSource,
    QuiescedSourceBatch, QuiescedSourceBatchProgress, SourceCloseBatch, SourceCloseBatchProgress,
    SourceRearmBatch, SourceRearmBatchProgress, V13MaintenanceEvent,
    domain_evidence::{DomainDecisionApplied, apply_domain_decision},
    domain_reclaim::{
        reclaim_domain_for_recovery, reclaim_domain_for_shutdown, retire_domain_recovery_sources,
    },
    reinit::DomainReinitPermitCell,
    request_runtime::{DomainRequestOwner, DomainRequestRuntime, RequestRuntimeBuildError},
    shutdown::{ControllerShutdown, DmaQuiescedLease, ParticipantId, ShutdownPhase},
    source::recovery::{DriverEvidenceRetireFailure, DriverEvidenceRoute},
    startup::{OwnerStartupCell, OwnerTransferCell},
};
use crate::maintenance::{
    DeviceMaintenanceHandle, MaintenanceCauses, MaintenanceClosed, MaintenancePublishResult,
    MaintenanceRegistrar, MaintenanceSession, MaintenanceSubmitError, MaintenanceThread,
    MaintenanceWaitOutcome, spawn_maintenance_domain,
};

const DOMAIN_SERVICE_BUDGET: usize = 64;
const DOMAIN_SERVICE_BATCH: NonZeroUsize =
    NonZeroUsize::new(DOMAIN_SERVICE_BUDGET).expect("domain service budget is nonzero");

fn retire_domain_driver_evidence(
    domain: &mut InstalledIoDomain,
    route: DriverEvidenceRoute,
    permit: RecoveryEvidenceRetirePermit,
) -> Result<RecoveryEvidenceRetired, DriverEvidenceRetireFailure> {
    if route != DriverEvidenceRoute::Io {
        return Err(DriverEvidenceRetireFailure::new(
            rdif_block::BlkError::Other(
                "control recovery evidence was routed to an independent I/O owner",
            ),
            permit,
        ));
    }
    domain
        .io_mut()
        .retire_recovery_evidence(permit)
        .map_err(DriverEvidenceRetireFailure::from_driver)
}

pub(super) struct InstalledDomainHandle {
    pub(super) proof: BoundDomainProof,
    pub(super) remote: DeviceMaintenanceHandle<V13MaintenanceEvent>,
    pub(super) thread: MaintenanceThread,
    pub(super) requests: Arc<DomainRequestRuntime>,
    pub(super) reinit: Arc<DomainReinitPermitCell>,
}

/// Exact platform-source capabilities transferred with one unbound domain.
///
/// The portable domain and these tokens cross to the final owner together.
/// A token is removed only when its matching IRQ action takes ownership.
pub(super) struct DomainPlatformSources {
    sources: Vec<ax_driver::ExactIrqSourceBinding>,
}

impl DomainPlatformSources {
    pub(super) const fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    pub(super) fn push(&mut self, source: ax_driver::ExactIrqSourceBinding) {
        self.sources.push(source);
    }

    pub(super) fn take(
        &mut self,
        source: rdif_block::IrqSourceId,
    ) -> Option<ax_driver::ExactIrqSourceBinding> {
        let index = self
            .sources
            .iter()
            .position(|candidate| candidate.source_id() == source.get())?;
        Some(self.sources.swap_remove(index))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

pub(super) enum RetainedDomainSpawnOwner {
    Unspawned {
        _domain: Box<UnboundIoDomain>,
        _platform_sources: Box<DomainPlatformSources>,
    },
    RequestRuntime {
        _domain: Box<UnboundIoDomain>,
        _platform_sources: Box<DomainPlatformSources>,
        _error: RequestRuntimeBuildError,
    },
    Running {
        _thread: MaintenanceThread,
    },
}

pub(super) struct DomainSpawnFailure {
    pub(super) phase: &'static str,
    pub(super) retained: Box<RetainedDomainSpawnOwner>,
}

pub(super) fn spawn_domain_owner(
    controller_name: &str,
    domain: UnboundIoDomain,
    platform_sources: DomainPlatformSources,
    topology: Arc<FixedOwnershipTopology>,
    shutdown: Arc<ControllerShutdown>,
    participant: ParticipantId,
    control_wake: DeviceMaintenanceHandle<V13MaintenanceEvent>,
) -> Result<InstalledDomainHandle, DomainSpawnFailure> {
    let domain_id = domain.domain_id();
    let Some(fixed_owner) = topology.domain(domain_id) else {
        return Err(DomainSpawnFailure {
            phase: "missing fixed domain topology",
            retained: Box::new(RetainedDomainSpawnOwner::Unspawned {
                _domain: Box::new(domain),
                _platform_sources: Box::new(platform_sources),
            }),
        });
    };
    let requests = match DomainRequestRuntime::new(
        domain_id,
        domain.queues(),
        crate::block::BlockRuntimeConfig::default(),
    ) {
        Ok(requests) => Arc::new(requests),
        Err(error) => {
            return Err(DomainSpawnFailure {
                phase: "construct final domain request runtime",
                retained: Box::new(RetainedDomainSpawnOwner::RequestRuntime {
                    _domain: Box::new(domain),
                    _platform_sources: Box::new(platform_sources),
                    _error: error,
                }),
            });
        }
    };
    let owner_cpu = fixed_owner.owner_cpu();
    let startup = Arc::new(OwnerStartupCell::new());
    let reinit = Arc::new(DomainReinitPermitCell::new(domain_id));
    let transfer = Arc::new(OwnerTransferCell::new((domain, platform_sources)));
    let child_startup = Arc::clone(&startup);
    let child_transfer = Arc::clone(&transfer);
    let child_topology = Arc::clone(&topology);
    let child_requests = Arc::clone(&requests);
    let child_shutdown = Arc::clone(&shutdown);
    let child_reinit = Arc::clone(&reinit);
    let name = format!("blk-v13/{controller_name}/domain-{}", domain_id.get());
    let thread = match spawn_maintenance_domain::<V13MaintenanceEvent, _>(
        owner_cpu,
        name,
        move |registrar| {
            let Some((domain, platform_sources)) = child_transfer.take() else {
                return quarantine_failed_domain_registration(
                    registrar,
                    child_startup,
                    "domain transfer owner missing",
                    (),
                );
            };
            run_domain_owner(
                DomainOwnerBootstrap {
                    domain,
                    platform_sources,
                    requests: child_requests,
                    topology: child_topology,
                    shutdown: child_shutdown,
                    participant,
                    control_wake,
                    reinit: child_reinit,
                    startup: child_startup,
                },
                registrar,
            )
        },
    ) {
        Ok(thread) => thread,
        Err(_) => {
            let retained = transfer
                .take()
                .expect("a failed spawn cannot consume the final-domain owner");
            return Err(DomainSpawnFailure {
                phase: "spawn final domain owner",
                retained: Box::new(RetainedDomainSpawnOwner::Unspawned {
                    _domain: Box::new(retained.0),
                    _platform_sources: Box::new(retained.1),
                }),
            });
        }
    };

    let result = match startup.wait_take() {
        Ok(result) => result,
        Err(_) => {
            return Err(DomainSpawnFailure {
                phase: "wait for final domain owner",
                retained: Box::new(RetainedDomainSpawnOwner::Running { _thread: thread }),
            });
        }
    };
    match result {
        Ok(ready) => Ok(InstalledDomainHandle {
            proof: ready.proof,
            remote: ready.remote,
            thread,
            requests,
            reinit,
        }),
        Err(failure) => Err(DomainSpawnFailure {
            phase: failure.phase,
            retained: Box::new(RetainedDomainSpawnOwner::Running { _thread: thread }),
        }),
    }
}

struct DomainOwnerBootstrap {
    domain: UnboundIoDomain,
    platform_sources: DomainPlatformSources,
    requests: Arc<DomainRequestRuntime>,
    topology: Arc<FixedOwnershipTopology>,
    shutdown: Arc<ControllerShutdown>,
    participant: ParticipantId,
    control_wake: DeviceMaintenanceHandle<V13MaintenanceEvent>,
    reinit: Arc<DomainReinitPermitCell>,
    startup: Arc<OwnerStartupCell<Result<DomainOwnerReady, DomainOwnerStartupFailure>>>,
}

fn run_domain_owner(
    bootstrap: DomainOwnerBootstrap,
    registrar: MaintenanceRegistrar<V13MaintenanceEvent>,
) -> Result<crate::maintenance::MaintenanceClosed, crate::maintenance::MaintenanceError> {
    let DomainOwnerBootstrap {
        mut domain,
        mut platform_sources,
        requests,
        topology,
        shutdown,
        participant,
        control_wake,
        reinit,
        startup,
    } = bootstrap;
    let domain_id = domain.domain_id();
    let owner = topology
        .domain(domain_id)
        .expect("the domain topology was validated before its owner spawned");
    let mut sources = Vec::new();
    for portable in domain.irq_sources_mut() {
        let IoDomainIrqSource::New(portable) = portable else {
            return quarantine_failed_domain_registration(
                registrar,
                startup,
                "independent domain contains an already-bound source",
                (domain, sources, platform_sources, topology),
            );
        };
        let source_id = portable.id();
        let platform_source = platform_sources.take(source_id);
        match BoundEvidenceSource::register_control_disabled(
            "block-domain",
            &registrar,
            owner,
            portable,
            platform_source,
        ) {
            Ok(source) => sources.push(source),
            Err(failure) => {
                return quarantine_failed_domain_registration(
                    registrar,
                    startup,
                    "register domain IRQ source",
                    (domain, sources, platform_sources, failure, topology),
                );
            }
        }
    }
    if !platform_sources.is_empty() {
        return quarantine_failed_domain_registration(
            registrar,
            startup,
            "match exact platform sources to portable domain endpoints",
            (domain, sources, platform_sources, topology),
        );
    }

    let remote = registrar.remote_handle();
    let owner_binding = match domain_owner_binding(registrar.owner_cpu(), registrar.owner_thread())
    {
        Ok(binding) => binding,
        Err(()) => {
            return quarantine_failed_domain_registration(
                registrar,
                startup,
                "construct nonzero domain owner identity",
                (domain, sources, topology),
            );
        }
    };
    let session = registrar.activate()?;
    for source in &sources {
        if let Err(error) = source.enable() {
            quarantine_failed_domain_session(
                session,
                startup,
                "enable domain IRQ action",
                (domain, sources, error, topology),
            );
        }
    }
    let (installed, proof) = match domain.finish_binding(owner_binding) {
        Ok(installed) => installed,
        Err(failure) => {
            quarantine_failed_domain_session(
                session,
                startup,
                "seal final domain binding",
                (failure, sources, topology),
            );
        }
    };
    let ready = DomainOwnerReady { proof, remote };
    if let Err(unaccepted) = startup.publish(Ok(ready)) {
        quarantine_failed_domain_session(
            session,
            startup,
            "publish final domain proof",
            (unaccepted, installed, sources, topology),
        );
    }
    Ok(service_domain_until_close(DomainServiceOwner {
        domain: installed,
        requests,
        sources,
        session,
        topology,
        shutdown,
        participant,
        control_wake,
        reinit,
    }))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DomainCycleProgress {
    Running,
    Freezing,
    DispatchStopped,
    SourcesStopped,
    Reclaimed,
    SourcesArmed,
    Resumed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DomainSourceStopMode {
    TerminalClose,
    Suspend,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DomainRunningTransition {
    None,
    ForwardDeferredShutdown,
}

struct DomainCycleState {
    progress: DomainCycleProgress,
    intent: Option<QuiesceIntent>,
    deferred_shutdown: bool,
    shutdown_forwarded: bool,
    recovery_requested: bool,
}

impl DomainCycleState {
    const fn running() -> Self {
        Self {
            progress: DomainCycleProgress::Running,
            intent: None,
            deferred_shutdown: false,
            shutdown_forwarded: false,
            recovery_requested: false,
        }
    }

    const fn progress(&self) -> DomainCycleProgress {
        self.progress
    }

    fn begin(&mut self, intent: QuiesceIntent) {
        debug_assert_eq!(self.progress, DomainCycleProgress::Running);
        self.intent = Some(intent);
        self.progress = DomainCycleProgress::Freezing;
        self.recovery_requested = matches!(intent, QuiesceIntent::Recovery(_));
        if intent == QuiesceIntent::Shutdown {
            self.deferred_shutdown = false;
            self.shutdown_forwarded = false;
        }
    }

    fn observe_shutdown_request(&mut self) {
        self.deferred_shutdown = true;
    }

    fn source_stop_mode(&self) -> DomainSourceStopMode {
        match self.intent {
            Some(QuiesceIntent::Recovery(_)) => DomainSourceStopMode::Suspend,
            Some(QuiesceIntent::Shutdown | QuiesceIntent::OwnershipTransfer) => {
                DomainSourceStopMode::TerminalClose
            }
            None => unreachable!("source stop requires an active lifecycle intent"),
        }
    }

    fn mark_dispatch_stopped(&mut self) {
        debug_assert_eq!(self.progress, DomainCycleProgress::Freezing);
        self.progress = DomainCycleProgress::DispatchStopped;
    }

    fn mark_sources_stopped(&mut self) {
        debug_assert_eq!(self.progress, DomainCycleProgress::DispatchStopped);
        self.progress = DomainCycleProgress::SourcesStopped;
    }

    fn mark_reclaimed(&mut self) {
        debug_assert_eq!(self.progress, DomainCycleProgress::SourcesStopped);
        self.progress = DomainCycleProgress::Reclaimed;
    }

    fn mark_sources_armed(&mut self) {
        debug_assert_eq!(self.progress, DomainCycleProgress::Reclaimed);
        self.progress = DomainCycleProgress::SourcesArmed;
    }

    fn mark_resumed(&mut self) {
        debug_assert_eq!(self.progress, DomainCycleProgress::SourcesArmed);
        self.progress = DomainCycleProgress::Resumed;
    }

    fn mark_recovery_requested(&mut self) {
        self.recovery_requested = true;
    }

    const fn recovery_requested(&self) -> bool {
        self.recovery_requested
    }

    fn accepts_recovery_request(&self) -> bool {
        self.progress == DomainCycleProgress::Running && !self.recovery_requested
    }

    const fn can_service_bound_sources(&self) -> bool {
        matches!(
            self.progress,
            DomainCycleProgress::Running
                | DomainCycleProgress::Freezing
                | DomainCycleProgress::DispatchStopped
        )
    }

    const fn can_dispatch(&self) -> bool {
        matches!(self.progress, DomainCycleProgress::Freezing)
            || (matches!(self.progress, DomainCycleProgress::Running) && !self.recovery_requested)
    }

    fn forward_shutdown_if_running(&mut self) -> DomainRunningTransition {
        if self.progress == DomainCycleProgress::Running
            && self.deferred_shutdown
            && !self.shutdown_forwarded
            && !self.recovery_requested
        {
            self.shutdown_forwarded = true;
            DomainRunningTransition::ForwardDeferredShutdown
        } else {
            DomainRunningTransition::None
        }
    }

    fn finish_recovered(&mut self) -> DomainRunningTransition {
        debug_assert_eq!(self.progress, DomainCycleProgress::Resumed);
        self.progress = DomainCycleProgress::Running;
        self.intent = None;
        self.recovery_requested = false;
        self.shutdown_forwarded = false;
        self.forward_shutdown_if_running()
    }
}

const fn watchdog_recovery_fault(expired: bool) -> Option<ControllerFault> {
    if expired {
        Some(ControllerFault::LostIrqEvidence)
    } else {
        None
    }
}

const fn next_domain_quiesce_epoch(active: ControllerEpoch) -> Option<ControllerEpoch> {
    match active.get().checked_add(1) {
        Some(next) => Some(ControllerEpoch::new(next)),
        None => None,
    }
}

struct DomainServiceOwner {
    domain: InstalledIoDomain,
    requests: Arc<DomainRequestRuntime>,
    sources: Vec<BoundEvidenceSource>,
    session: MaintenanceSession<V13MaintenanceEvent>,
    topology: Arc<FixedOwnershipTopology>,
    shutdown: Arc<ControllerShutdown>,
    participant: ParticipantId,
    control_wake: DeviceMaintenanceHandle<V13MaintenanceEvent>,
    reinit: Arc<DomainReinitPermitCell>,
}

fn service_domain_until_close(owner: DomainServiceOwner) -> MaintenanceClosed {
    let DomainServiceOwner {
        mut domain,
        requests,
        mut sources,
        session,
        topology,
        shutdown,
        mut participant,
        control_wake,
        reinit,
    } = owner;
    let participant_index = participant.index();
    let mut request_owner = DomainRequestOwner::new(requests);
    let mut cycle = DomainCycleState::running();
    let mut closed_recovery_sources = Vec::new();
    let mut contained_fault_sources: Vec<QuiescedEvidenceSource> = Vec::new();
    let mut quiesced_sources: Option<QuiescedSourceBatch> = None;
    let mut terminal_source_close: Option<SourceCloseBatch> = None;
    let mut rearm_sources: Option<SourceRearmBatch> = None;
    let mut dma_lease: Option<DmaQuiescedLease> = None;
    loop {
        let snapshot = shutdown.snapshot();
        if snapshot.phase() == ShutdownPhase::Quarantined {
            quarantine_domain_session(
                session,
                "controller shutdown transaction quarantined",
                (domain, sources, request_owner, shutdown, topology),
            );
        }
        if snapshot.phase() == ShutdownPhase::Running
            && cycle.progress() == DomainCycleProgress::Resumed
        {
            participant = match shutdown.participant(participant_index) {
                Ok(participant) => participant,
                Err(error) => quarantine_domain_session(
                    session,
                    "refresh domain participant for recovered lifecycle cycle",
                    (domain, sources, request_owner, error, shutdown, topology),
                ),
            };
            if cycle.finish_recovered() == DomainRunningTransition::ForwardDeferredShutdown
                && let Err(error) = control_wake.publish_cause(MaintenanceCauses::SHUTDOWN)
            {
                quarantine_domain_session(
                    session,
                    "forward deferred shutdown after controller recovery",
                    (domain, sources, request_owner, error, shutdown, topology),
                );
            }
        }
        if snapshot.phase() == ShutdownPhase::Freezing
            && cycle.progress() == DomainCycleProgress::Running
        {
            let Some(intent) = snapshot.intent() else {
                quarantine_domain_session(
                    session,
                    "begin domain lifecycle without a controller intent",
                    (domain, sources, request_owner, shutdown, topology),
                );
            };
            if let Err(error) = request_owner.begin_quiesce() {
                quarantine_domain_session(
                    session,
                    "freeze domain admission and begin dispatch cutoff",
                    (domain, sources, request_owner, error, shutdown, topology),
                );
            }
            cycle.begin(intent);
        }
        let mut budget_used = 0;
        if cycle.can_service_bound_sources() {
            while budget_used < DOMAIN_SERVICE_BUDGET {
                let Some((source_index, fault)) =
                    take_first_owner(&mut sources, |source| source.take_fault())
                else {
                    break;
                };
                let Some(quiesce_epoch) = next_domain_quiesce_epoch(domain.epoch()) else {
                    quarantine_domain_session(
                        session,
                        "advance controller epoch for contained IRQ fault recovery",
                        (domain, sources, request_owner, fault, topology),
                    );
                };
                let source = sources.remove(source_index);
                match source.suspend_contained_fault_after_mask(
                    fault,
                    domain.controller_identity(),
                    quiesce_epoch,
                    crate::block::activation_v13::source::recovery::DriverEvidenceRoute::Io,
                ) {
                    Ok(source) => contained_fault_sources.push(source),
                    Err(failure) => {
                        let (reason, source, fault) = failure.into_parts();
                        sources.insert(source_index, source);
                        quarantine_domain_session(
                            session,
                            "suspend contained domain IRQ source fault",
                            (
                                domain,
                                sources,
                                contained_fault_sources,
                                request_owner,
                                reason,
                                fault,
                                topology,
                            ),
                        );
                    }
                }
                if let Err(error) = request_controller_recovery(
                    &control_wake,
                    &mut cycle,
                    ControllerFault::Ownership,
                ) {
                    quarantine_domain_session(
                        session,
                        "wake control owner for contained domain IRQ fault",
                        (
                            domain,
                            sources,
                            contained_fault_sources,
                            request_owner,
                            error,
                            topology,
                        ),
                    );
                }
                budget_used += 1;
            }
            while budget_used < DOMAIN_SERVICE_BUDGET {
                let Some((source_index, pending)) =
                    take_first_owner(&mut sources, |source| source.take_pending())
                else {
                    break;
                };
                let evidence_id = pending.evidence_id();
                let decision = match domain
                    .io_mut()
                    .service_evidence(evidence_id, request_owner.completion_sink())
                {
                    Ok(EvidenceServiceResult::Drained) => pending.drain(),
                    Ok(EvidenceServiceResult::Retained) => pending.retain(),
                    Ok(EvidenceServiceResult::Recover(fault)) => pending.recover(fault),
                    Err(error) => {
                        quarantine_domain_session(
                            session,
                            "portable domain evidence service",
                            (domain, sources, request_owner, pending, error, topology),
                        );
                    }
                };
                let completed = match request_owner.finish_completions() {
                    Ok(completed) => completed,
                    Err(error) => quarantine_domain_session(
                        session,
                        "publish domain terminal completions",
                        (domain, sources, request_owner, decision, error, topology),
                    ),
                };
                match apply_domain_decision(
                    &mut sources[source_index],
                    decision,
                    domain.controller_identity(),
                    crate::block::activation_v13::source::recovery::DriverEvidenceRoute::Io,
                    |evidence| domain.io_mut().commit_drained_evidence(evidence),
                ) {
                    Ok(DomainDecisionApplied::RecoveryRequired(fault)) => {
                        if let Err(error) =
                            request_controller_recovery(&control_wake, &mut cycle, fault)
                        {
                            quarantine_domain_session(
                                session,
                                "wake control owner for domain recovery",
                                (domain, sources, request_owner, error, topology),
                            );
                        }
                    }
                    Ok(
                        DomainDecisionApplied::EvidenceRetained
                        | DomainDecisionApplied::EvidenceDrained,
                    ) => {}
                    Err(failure) => quarantine_domain_session(
                        session,
                        "apply domain IRQ evidence disposition",
                        (domain, sources, request_owner, failure, topology),
                    ),
                }
                budget_used = budget_used.saturating_add(completed.max(1));
            }
            if budget_used < DOMAIN_SERVICE_BUDGET && cycle.can_dispatch() {
                match request_owner.dispatch(domain.io_mut(), DOMAIN_SERVICE_BUDGET - budget_used) {
                    Ok(dispatched) => budget_used += dispatched,
                    Err(error) => quarantine_domain_session(
                        session,
                        "dispatch domain staged request",
                        (domain, sources, request_owner, error, topology),
                    ),
                }
            }
        }
        let mut mailbox_recovery = None;
        let drain = match session.drain_owner(DOMAIN_SERVICE_BUDGET, |event| {
            if let V13MaintenanceEvent::Recovery { fault } = event {
                mailbox_recovery.get_or_insert(fault);
            }
        }) {
            Ok(drain) => drain,
            Err(error) => quarantine_domain_session(
                session,
                "drain domain maintenance mailbox",
                (domain, sources, request_owner, error, topology),
            ),
        };
        if let Some(fault) = mailbox_recovery
            && let Err(error) = request_controller_recovery(&control_wake, &mut cycle, fault)
        {
            quarantine_domain_session(
                session,
                "forward mailbox recovery to control owner",
                (domain, sources, request_owner, error, topology),
            );
        }
        if drain.causes().contains(MaintenanceCauses::SHUTDOWN) {
            cycle.observe_shutdown_request();
        }
        if shutdown.snapshot().phase() == ShutdownPhase::Running
            && cycle.forward_shutdown_if_running()
                == DomainRunningTransition::ForwardDeferredShutdown
            && let Err(error) = control_wake.publish_cause(MaintenanceCauses::SHUTDOWN)
        {
            quarantine_domain_session(
                session,
                "forward domain shutdown request to control owner",
                (domain, sources, request_owner, error, topology),
            );
        }
        if cycle.progress() == DomainCycleProgress::Freezing {
            match request_owner.try_commit_quiesced() {
                Ok(true) => {
                    if let Err(error) = shutdown.ack_dispatch_cutoff(participant) {
                        quarantine_domain_session(
                            session,
                            "acknowledge domain dispatch cutoff",
                            (domain, sources, request_owner, error, shutdown, topology),
                        );
                    }
                    if let Err(error) = control_wake.publish_cause(MaintenanceCauses::LIFECYCLE) {
                        quarantine_domain_session(
                            session,
                            "wake control owner after domain dispatch cutoff",
                            (domain, sources, request_owner, error, shutdown, topology),
                        );
                    }
                    cycle.mark_dispatch_stopped();
                }
                Ok(false) => {}
                Err(error) => quarantine_domain_session(
                    session,
                    "commit domain dispatch cutoff",
                    (domain, sources, request_owner, error, shutdown, topology),
                ),
            }
        }
        if shutdown.snapshot().phase() == ShutdownPhase::DeviceMasked
            && cycle.progress() == DomainCycleProgress::DispatchStopped
        {
            let Some(quiesce_epoch) = next_domain_quiesce_epoch(domain.epoch()) else {
                quarantine_domain_session(
                    session,
                    "advance controller epoch for domain source suspension",
                    (domain, sources, request_owner, shutdown, topology),
                );
            };
            let mut suspended_sources = mem::take(&mut contained_fault_sources);
            match cycle.source_stop_mode() {
                DomainSourceStopMode::TerminalClose => {
                    if !suspended_sources.is_empty() {
                        // The fault transaction is already cleanly suspended,
                        // but terminal close still needs the matching DMA proof
                        // before destroying its retained action and endpoint.
                        quiesced_sources = Some(QuiescedSourceBatch::new(suspended_sources));
                    }
                    while let Some(source) = sources.pop() {
                        match source.close_after_mask() {
                            Ok(ClosedSourceDisposition::Closed) => {}
                            Ok(ClosedSourceDisposition::Recovery(source)) => {
                                closed_recovery_sources.push(source);
                            }
                            Err(failure) => {
                                let (reason, source) = failure.into_parts();
                                sources.push(source);
                                quarantine_domain_session(
                                    session,
                                    "close domain IRQ sources",
                                    (domain, sources, request_owner, reason, shutdown, topology),
                                );
                            }
                        }
                    }
                }
                DomainSourceStopMode::Suspend => {
                    while let Some(source) = sources.pop() {
                        match source.suspend_after_mask(
                            domain.controller_identity(),
                            quiesce_epoch,
                        ) {
                            Ok(source) => suspended_sources.push(source),
                            Err(failure) => {
                                let (reason, source) = failure.into_parts();
                                sources.push(source);
                                quarantine_domain_session(
                                    session,
                                    "suspend domain IRQ sources for recovery",
                                    (
                                        domain,
                                        sources,
                                        suspended_sources,
                                        request_owner,
                                        reason,
                                        shutdown,
                                        topology,
                                    ),
                                );
                            }
                        }
                    }
                    quiesced_sources = Some(QuiescedSourceBatch::new(suspended_sources));
                }
            }
            if let Err(error) = shutdown.ack_sources_closed(participant) {
                quarantine_domain_session(
                    session,
                    "acknowledge closed domain IRQ sources",
                    (domain, request_owner, error, shutdown, topology),
                );
            }
            if let Err(error) = control_wake.publish_cause(MaintenanceCauses::LIFECYCLE) {
                quarantine_domain_session(
                    session,
                    "wake control owner after domain source close",
                    (domain, request_owner, error, shutdown, topology),
                );
            }
            cycle.mark_sources_stopped();
        }
        if shutdown.snapshot().phase() == ShutdownPhase::DmaQuiesced
            && cycle.progress() == DomainCycleProgress::SourcesStopped
        {
            if dma_lease.is_none() {
                dma_lease = Some(match shutdown.borrow_dma_quiesced(participant) {
                    Ok(lease) => lease,
                    Err(error) => quarantine_domain_session(
                        session,
                        "borrow controller DMA proof for domain reclaim",
                        (domain, request_owner, error, shutdown, topology),
                    ),
                });
            }
            let proof = dma_lease
                .as_ref()
                .expect("the domain participant retains its DMA proof lease")
                .proof();
            match cycle.source_stop_mode() {
                DomainSourceStopMode::TerminalClose => {
                    if let Some(batch) = quiesced_sources.take() {
                        match batch.advance(proof, DOMAIN_SERVICE_BATCH, |route, permit| {
                            retire_domain_driver_evidence(&mut domain, route, permit)
                        }) {
                            Ok(QuiescedSourceBatchProgress::More(batch)) => {
                                quiesced_sources = Some(batch);
                                let _ = crate::task::yield_current_cpu();
                                continue;
                            }
                            Ok(QuiescedSourceBatchProgress::Ready(batch)) => {
                                terminal_source_close = Some(match batch.choose_terminal_close() {
                                    Ok(batch) => batch,
                                    Err(failure) => quarantine_domain_session(
                                        session,
                                        "select terminal close for contained-fault sources",
                                        (
                                            domain,
                                            sources,
                                            request_owner,
                                            failure,
                                            dma_lease,
                                            shutdown,
                                            topology,
                                        ),
                                    ),
                                });
                            }
                            Err(failure) => quarantine_domain_session(
                                session,
                                "retire contained-fault sources for terminal close",
                                (
                                    domain,
                                    sources,
                                    request_owner,
                                    failure,
                                    dma_lease,
                                    shutdown,
                                    topology,
                                ),
                            ),
                        }
                    }
                    if let Some(batch) = terminal_source_close.take() {
                        match batch.advance(DOMAIN_SERVICE_BATCH) {
                            Ok(SourceCloseBatchProgress::More(batch)) => {
                                terminal_source_close = Some(batch);
                                let _ = crate::task::yield_current_cpu();
                                continue;
                            }
                            Ok(SourceCloseBatchProgress::Closed) => {}
                            Err(failure) => quarantine_domain_session(
                                session,
                                "close contained-fault sources after DMA quiescence",
                                (
                                    domain,
                                    sources,
                                    request_owner,
                                    failure,
                                    dma_lease,
                                    shutdown,
                                    topology,
                                ),
                            ),
                        }
                    }
                    match retire_domain_recovery_sources(
                        &mut closed_recovery_sources,
                        proof,
                        |route, permit| {
                            retire_domain_driver_evidence(&mut domain, route, permit)
                        },
                    ) {
                        Ok(true) => {}
                        Ok(false) => {
                            let _ = crate::task::yield_current_cpu();
                            continue;
                        }
                        Err(reason) => quarantine_domain_session(
                            session,
                            "retire recovery-bound domain IRQ evidence",
                            (
                                domain,
                                request_owner,
                                closed_recovery_sources,
                                dma_lease,
                                reason,
                                shutdown,
                                topology,
                            ),
                        ),
                    }
                    if let Err(error) =
                        reclaim_domain_for_shutdown(&mut domain, &mut request_owner, proof)
                    {
                        quarantine_domain_session(
                            session,
                            "reclaim and close quiesced domain",
                            (domain, request_owner, dma_lease, error, shutdown, topology),
                        );
                    }
                }
                DomainSourceStopMode::Suspend => {
                    let batch = quiesced_sources
                        .take()
                        .expect("recovery source suspension published one complete batch");
                    match batch.advance(proof, DOMAIN_SERVICE_BATCH, |route, permit| {
                        retire_domain_driver_evidence(&mut domain, route, permit)
                    }) {
                        Ok(QuiescedSourceBatchProgress::More(batch)) => {
                            quiesced_sources = Some(batch);
                            let _ = crate::task::yield_current_cpu();
                            continue;
                        }
                        Ok(QuiescedSourceBatchProgress::Ready(batch)) => {
                            rearm_sources = Some(batch);
                        }
                        Err(failure) => quarantine_domain_session(
                            session,
                            "retire suspended domain IRQ evidence",
                            (
                                domain,
                                request_owner,
                                failure,
                                dma_lease,
                                shutdown,
                                topology,
                            ),
                        ),
                    }
                    if let Err(error) =
                        reclaim_domain_for_recovery(&mut domain, &mut request_owner, proof)
                    {
                        quarantine_domain_session(
                            session,
                            "reclaim quiesced domain for controller recovery",
                            (domain, request_owner, error, dma_lease, shutdown, topology),
                        );
                    }
                }
            }
            let lease = dma_lease
                .take()
                .expect("the domain participant owns one DMA proof lease");
            if let Err(failure) = shutdown.ack_reclaimed(lease) {
                quarantine_domain_session(
                    session,
                    "acknowledge domain resource reclaim",
                    (domain, request_owner, failure, shutdown, topology),
                );
            }
            if let Err(error) = control_wake.publish_cause(MaintenanceCauses::LIFECYCLE) {
                quarantine_domain_session(
                    session,
                    "wake control owner after domain reclaim",
                    (domain, request_owner, error, shutdown, topology),
                );
            }
            if cycle.source_stop_mode() == DomainSourceStopMode::TerminalClose {
                return close_domain_session(session, domain, request_owner, shutdown, topology);
            }
            cycle.mark_reclaimed();
        }
        if shutdown.snapshot().phase() == ShutdownPhase::ReinitSourcesArming
            && cycle.progress() == DomainCycleProgress::Reclaimed
        {
            let batch = rearm_sources
                .take()
                .expect("DMA-quiesced recovery produced one source re-arm batch");
            match batch.advance(DOMAIN_SERVICE_BATCH) {
                Ok(SourceRearmBatchProgress::More(batch)) => {
                    rearm_sources = Some(batch);
                    let _ = crate::task::yield_current_cpu();
                    continue;
                }
                Ok(SourceRearmBatchProgress::Armed(restored)) => sources = restored,
                Err(failure) => quarantine_domain_session(
                    session,
                    "re-arm retained domain IRQ actions",
                    (domain, request_owner, failure, shutdown, topology),
                ),
            }
            if let Err(error) = shutdown.ack_reinit_sources_armed(participant) {
                quarantine_domain_session(
                    session,
                    "acknowledge re-armed domain IRQ sources",
                    (domain, sources, request_owner, error, shutdown, topology),
                );
            }
            if let Err(error) = control_wake.publish_cause(MaintenanceCauses::LIFECYCLE) {
                quarantine_domain_session(
                    session,
                    "wake control owner after domain IRQ re-arm",
                    (domain, sources, request_owner, error, shutdown, topology),
                );
            }
            cycle.mark_sources_armed();
        }
        if shutdown.snapshot().phase() == ShutdownPhase::OwnersResuming
            && cycle.progress() == DomainCycleProgress::SourcesArmed
            && let Some(permit) = reinit.take_permit()
        {
            let resumed = match domain.resume_after_reinitialize(permit) {
                Ok(resumed) => resumed,
                Err(failure) => quarantine_domain_session(
                    session,
                    "resume portable domain after controller reinitialization",
                    (domain, sources, request_owner, failure, shutdown, topology),
                ),
            };
            if let Err(error) = request_owner.resume_after_reinitialize() {
                quarantine_domain_session(
                    session,
                    "resume domain request gates after controller reinitialization",
                    (
                        domain,
                        sources,
                        request_owner,
                        resumed,
                        error,
                        shutdown,
                        topology,
                    ),
                );
            }
            if let Err(failure) = reinit.publish_resumed(resumed) {
                quarantine_domain_session(
                    session,
                    "return resumed-domain proof to controller owner",
                    (domain, sources, request_owner, failure, shutdown, topology),
                );
            }
            if let Err(error) = shutdown.ack_resumed(participant) {
                quarantine_domain_session(
                    session,
                    "acknowledge resumed domain owner",
                    (domain, sources, request_owner, error, shutdown, topology),
                );
            }
            if let Err(error) = control_wake.publish_cause(MaintenanceCauses::LIFECYCLE) {
                quarantine_domain_session(
                    session,
                    "wake control owner after domain resume",
                    (domain, sources, request_owner, error, shutdown, topology),
                );
            }
            cycle.mark_resumed();
        }
        if budget_used >= DOMAIN_SERVICE_BUDGET
            || drain.pending()
            || (request_owner.has_staged() && cycle.can_dispatch())
        {
            let _ = crate::task::yield_current_cpu();
            continue;
        }
        if cycle.progress() == DomainCycleProgress::Running
            && !cycle.recovery_requested()
            && let Some(deadline) = request_owner.earliest_deadline()
        {
            match session.wait_for_pending_until(deadline) {
                Ok(MaintenanceWaitOutcome::ConditionMet) => {}
                Ok(MaintenanceWaitOutcome::TimedOut) => {
                    let expired = request_owner.has_expired(ax_hal::time::monotonic_time_nanos());
                    if let Some(fault) = watchdog_recovery_fault(expired)
                        && let Err(error) =
                            request_controller_recovery(&control_wake, &mut cycle, fault)
                    {
                        quarantine_domain_session(
                            session,
                            "wake control owner after domain request watchdog",
                            (domain, sources, request_owner, error, topology),
                        );
                    }
                }
                Err(error) => quarantine_domain_session(
                    session,
                    "wait for domain deadline or evidence",
                    (domain, sources, request_owner, error, topology),
                ),
            }
        } else if let Err(error) = session.wait_for_pending() {
            quarantine_domain_session(
                session,
                "wait for domain evidence",
                (domain, sources, request_owner, error, topology),
            );
        }
    }
}

fn request_controller_recovery(
    control_wake: &DeviceMaintenanceHandle<V13MaintenanceEvent>,
    cycle: &mut DomainCycleState,
    fault: ControllerFault,
) -> Result<(), DomainRecoveryRequestError> {
    if !cycle.accepts_recovery_request() {
        return Ok(());
    }
    validate_recovery_publication(control_wake.submit_request(
        MaintenanceCauses::LIFECYCLE,
        V13MaintenanceEvent::Recovery { fault },
    )?)?;
    cycle.mark_recovery_requested();
    Ok(())
}

const fn validate_recovery_publication(
    publication: MaintenancePublishResult,
) -> Result<(), DomainRecoveryRequestError> {
    match publication {
        MaintenancePublishResult::Published => {}
        MaintenancePublishResult::Overflowed => {
            return Err(DomainRecoveryRequestError::MailboxOverflow);
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum DomainRecoveryRequestError {
    #[error(transparent)]
    Submit(#[from] MaintenanceSubmitError),
    #[error("controller recovery request overflowed its maintenance mailbox")]
    MailboxOverflow,
}

fn close_domain_session(
    session: MaintenanceSession<V13MaintenanceEvent>,
    domain: InstalledIoDomain,
    request_owner: DomainRequestOwner,
    shutdown: Arc<ControllerShutdown>,
    topology: Arc<FixedOwnershipTopology>,
) -> MaintenanceClosed {
    if let Err(error) = session.begin_close() {
        quarantine_domain_session(
            session,
            "close domain publication admission",
            (domain, request_owner, error, shutdown, topology),
        );
    }
    loop {
        let drain = match session.drain_owner(DOMAIN_SERVICE_BUDGET, |_| {}) {
            Ok(drain) => drain,
            Err(error) => quarantine_domain_session(
                session,
                "drain domain mailbox during close",
                (domain, request_owner, error, shutdown, topology),
            ),
        };
        if !drain.pending() {
            break;
        }
    }
    if let Err(error) = session.try_begin_draining() {
        quarantine_domain_session(
            session,
            "enter domain maintenance drain",
            (domain, request_owner, error, shutdown, topology),
        );
    }
    if let Err(error) = session.finish_close() {
        quarantine_domain_session(
            session,
            "commit domain maintenance close",
            (domain, request_owner, error, shutdown, topology),
        );
    }
    match session.try_into_closed() {
        Ok(closed) => closed,
        Err(failure) => {
            let error = failure.error();
            quarantine_domain_session(
                failure.into_session(),
                "extract domain maintenance close proof",
                (domain, request_owner, error, shutdown, topology),
            )
        }
    }
}

pub(super) fn take_first_owner<T, O>(
    slots: &mut [T],
    mut take: impl FnMut(&mut T) -> Option<O>,
) -> Option<(usize, O)> {
    slots
        .iter_mut()
        .enumerate()
        .find_map(|(index, slot)| take(slot).map(|owner| (index, owner)))
}

pub(super) fn domain_owner_binding(
    cpu: usize,
    thread: crate::task::ThreadId,
) -> Result<DomainOwnerBinding, ()> {
    let cpu = u32::try_from(cpu).map_err(|_| ())?;
    let thread_cookie = NonZeroU64::new(thread.as_u64()).ok_or(())?;
    Ok(DomainOwnerBinding::new(cpu, thread_cookie))
}

struct DomainOwnerReady {
    proof: BoundDomainProof,
    remote: DeviceMaintenanceHandle<V13MaintenanceEvent>,
}

struct DomainOwnerStartupFailure {
    phase: &'static str,
}

fn quarantine_failed_domain_registration<T>(
    registrar: MaintenanceRegistrar<V13MaintenanceEvent>,
    startup: Arc<OwnerStartupCell<Result<DomainOwnerReady, DomainOwnerStartupFailure>>>,
    phase: &'static str,
    retained: T,
) -> Result<crate::maintenance::MaintenanceClosed, crate::maintenance::MaintenanceError> {
    let session = registrar.activate()?;
    quarantine_failed_domain_session(session, startup, phase, retained)
}

fn quarantine_failed_domain_session<T>(
    session: MaintenanceSession<V13MaintenanceEvent>,
    startup: Arc<OwnerStartupCell<Result<DomainOwnerReady, DomainOwnerStartupFailure>>>,
    phase: &'static str,
    retained: T,
) -> ! {
    let _ = startup.publish(Err(DomainOwnerStartupFailure { phase }));
    quarantine_domain_session(session, phase, retained)
}

fn quarantine_domain_session<T>(
    session: MaintenanceSession<V13MaintenanceEvent>,
    phase: &'static str,
    retained: T,
) -> ! {
    error!("block v0.13 domain entered quarantine during {phase}");
    let _retained = retained;
    session.quarantine_and_park()
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn owner_lookup_moves_the_first_ready_owner_exactly_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut slots = [
            None,
            Some(DropOwner {
                identity: 41,
                drops: Arc::clone(&drops),
            }),
            Some(DropOwner {
                identity: 73,
                drops: Arc::clone(&drops),
            }),
        ];

        let (index, owner) = take_first_owner(&mut slots, Option::take).unwrap();

        assert_eq!(index, 1);
        assert_eq!(owner.identity, 41);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert!(slots[0].is_none());
        assert!(slots[1].is_none());
        assert_eq!(slots[2].as_ref().map(|owner| owner.identity), Some(73));
        drop(owner);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn recovery_defers_external_shutdown_until_the_new_cycle_is_running() {
        let mut cycle = DomainCycleState::running();
        cycle.observe_shutdown_request();
        cycle.begin(QuiesceIntent::Recovery(ControllerFault::LostIrqEvidence));

        assert_eq!(cycle.source_stop_mode(), DomainSourceStopMode::Suspend);
        cycle.mark_dispatch_stopped();
        cycle.mark_sources_stopped();
        cycle.mark_reclaimed();
        cycle.mark_sources_armed();
        cycle.mark_resumed();

        assert_eq!(
            cycle.finish_recovered(),
            DomainRunningTransition::ForwardDeferredShutdown
        );
        assert_eq!(cycle.progress(), DomainCycleProgress::Running);
    }

    #[test]
    fn accepted_recovery_request_wins_over_a_same_cycle_shutdown_wake() {
        let mut cycle = DomainCycleState::running();
        cycle.mark_recovery_requested();
        cycle.observe_shutdown_request();

        assert_eq!(
            cycle.forward_shutdown_if_running(),
            DomainRunningTransition::None
        );

        cycle.begin(QuiesceIntent::Recovery(ControllerFault::Protocol));
        cycle.mark_dispatch_stopped();
        cycle.mark_sources_stopped();
        cycle.mark_reclaimed();
        cycle.mark_sources_armed();
        cycle.mark_resumed();
        assert_eq!(
            cycle.finish_recovered(),
            DomainRunningTransition::ForwardDeferredShutdown
        );
    }

    #[test]
    fn shutdown_and_recovery_choose_distinct_source_owner_transitions() {
        let mut shutdown = DomainCycleState::running();
        shutdown.begin(QuiesceIntent::Shutdown);
        assert_eq!(
            shutdown.source_stop_mode(),
            DomainSourceStopMode::TerminalClose
        );

        let mut recovery = DomainCycleState::running();
        recovery.begin(QuiesceIntent::Recovery(ControllerFault::Protocol));
        assert_eq!(recovery.source_stop_mode(), DomainSourceStopMode::Suspend);
    }

    #[test]
    fn watchdog_expiry_maps_to_lost_irq_recovery_instead_of_quarantine() {
        assert_eq!(
            watchdog_recovery_fault(true),
            Some(ControllerFault::LostIrqEvidence)
        );
        assert_eq!(watchdog_recovery_fault(false), None);
    }

    #[test]
    fn contained_irq_fault_uses_the_next_domain_epoch() {
        assert_eq!(
            next_domain_quiesce_epoch(ControllerEpoch::INITIAL),
            Some(ControllerEpoch::new(ControllerEpoch::INITIAL.get() + 1))
        );
    }

    #[test]
    fn contained_irq_fault_does_not_wrap_an_exhausted_domain_epoch() {
        assert_eq!(
            next_domain_quiesce_epoch(ControllerEpoch::new(u64::MAX)),
            None
        );
    }

    #[test]
    fn recovery_request_requires_its_fault_event_to_reach_the_control_mailbox() {
        assert!(validate_recovery_publication(MaintenancePublishResult::Published).is_ok());
        assert!(matches!(
            validate_recovery_publication(MaintenancePublishResult::Overflowed),
            Err(DomainRecoveryRequestError::MailboxOverflow)
        ));
    }

    struct DropOwner {
        identity: usize,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for DropOwner {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
}
