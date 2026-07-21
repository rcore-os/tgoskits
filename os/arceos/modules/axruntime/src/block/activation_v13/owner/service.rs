//! Ready-state request routing and evidence service on the control owner.

use rdif_block::{DomainReinitPermit, DomainResumeFailure, DomainResumed};

use super::{
    control_close::{close_control_session, quarantine_control_owner},
    lifecycle::{ControlLifecycle, ControlLifecycleAdvance},
    *,
};
use crate::block::activation_v13::domain_reclaim::{
    DomainRecoveryReclaimError, DomainTerminalCloseError, close_reclaimed_domain,
    reclaim_domain_for_recovery,
};

pub(super) struct RequestRuntimeInstallation {
    pub(super) devices: Box<[V13BlockDeviceView]>,
    pub(super) combined_requests: Option<DomainRequestOwner>,
}

pub(super) enum ControlIoRuntime {
    None,
    Split(InstalledSharedDomain),
    Combined(DomainRequestOwner),
}

impl ControlIoRuntime {
    fn requests(&self) -> Option<&DomainRequestOwner> {
        match self {
            Self::None => None,
            Self::Split(shared) => Some(&shared.requests),
            Self::Combined(requests) => Some(requests),
        }
    }

    pub(super) fn has_staged(&self) -> bool {
        self.requests().is_some_and(DomainRequestOwner::has_staged)
    }

    pub(super) fn earliest_deadline(&self) -> Option<u64> {
        self.requests()
            .and_then(DomainRequestOwner::earliest_deadline)
    }

    pub(super) fn has_expired(&self, now_ns: u64) -> bool {
        self.requests()
            .is_some_and(|requests| requests.has_expired(now_ns))
    }

    pub(super) fn evidence_route(
        &self,
        source: rdif_block::IrqSourceId,
    ) -> crate::block::activation_v13::source::recovery::DriverEvidenceRoute {
        if self
            .requests()
            .is_some_and(|requests| requests.handles_source(source))
        {
            crate::block::activation_v13::source::recovery::DriverEvidenceRoute::Io
        } else {
            crate::block::activation_v13::source::recovery::DriverEvidenceRoute::Control
        }
    }

    pub(super) fn retire_recovery_evidence(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        route: crate::block::activation_v13::source::recovery::DriverEvidenceRoute,
        permit: rdif_block::RecoveryEvidenceRetirePermit,
    ) -> Result<
        rdif_block::RecoveryEvidenceRetired,
        crate::block::activation_v13::source::recovery::DriverEvidenceRetireFailure,
    > {
        use crate::block::activation_v13::source::recovery::{
            DriverEvidenceRetireFailure, DriverEvidenceRoute,
        };

        let result = match route {
            DriverEvidenceRoute::Control => published
                .published_mut()
                .control_mut()
                .retire_recovery_evidence(permit),
            DriverEvidenceRoute::Io => match self {
                Self::Split(shared) => shared
                    .domain
                    .io_mut()
                    .retire_recovery_evidence(permit),
                Self::Combined(_) => {
                    let Some(mut domain) = published.published_mut().shared_io_domain_mut() else {
                        return Err(DriverEvidenceRetireFailure::new(
                            rdif_block::BlkError::Other(
                                "combined I/O owner unavailable during recovery retirement",
                            ),
                            permit,
                        ));
                    };
                    domain.retire_recovery_evidence(permit)
                }
                Self::None => {
                    return Err(DriverEvidenceRetireFailure::new(
                        rdif_block::BlkError::Other(
                            "I/O recovery evidence routed to a control-only owner",
                        ),
                        permit,
                    ));
                }
            },
        };
        result.map_err(DriverEvidenceRetireFailure::from_driver)
    }

    pub(super) fn begin_quiesce(&self) -> Result<(), DomainRequestLifecycleError> {
        if let Some(requests) = self.requests() {
            requests.begin_quiesce()?;
        }
        Ok(())
    }

    pub(super) fn try_commit_quiesced(&self) -> Result<bool, DomainRequestLifecycleError> {
        self.requests()
            .map_or(Ok(true), DomainRequestOwner::try_commit_quiesced)
    }

    pub(super) fn reclaim_for_recovery(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        proof: &DmaQuiesced,
    ) -> Result<(), ControlIoReclaimError> {
        match self {
            Self::None => Ok(()),
            Self::Split(shared) => {
                reclaim_domain_for_recovery(shared.domain.io_mut(), &mut shared.requests, proof)
                    .map_err(ControlIoReclaimError::Recovery)
            }
            Self::Combined(requests) => {
                let mut domain = published
                    .published_mut()
                    .shared_io_domain_mut()
                    .ok_or(ControlIoReclaimError::CombinedUnavailable)?;
                reclaim_domain_for_recovery(&mut domain, requests, proof)
                    .map_err(ControlIoReclaimError::Recovery)
            }
        }
    }

    pub(super) fn close_reclaimed(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
    ) -> Result<(), ControlIoReclaimError> {
        match self {
            Self::None => Ok(()),
            Self::Split(shared) => close_reclaimed_domain(shared.domain.io_mut(), &shared.requests)
                .map_err(ControlIoReclaimError::Close),
            Self::Combined(requests) => {
                let mut domain = published
                    .published_mut()
                    .shared_io_domain_mut()
                    .ok_or(ControlIoReclaimError::CombinedUnavailable)?;
                close_reclaimed_domain(&mut domain, requests).map_err(ControlIoReclaimError::Close)
            }
        }
    }

    pub(super) fn domain_id(
        &self,
        published: &RdifBlockPublishedOwner,
    ) -> Option<rdif_block::OwnershipDomainId> {
        match self {
            Self::None => None,
            Self::Split(shared) => Some(shared.domain.domain_id()),
            Self::Combined(_) => published
                .published()
                .shared_io_queues()
                .and_then(|queues| queues.first())
                .map(rdif_block::InterruptQueueDesc::ownership_domain),
        }
    }

    pub(super) fn resume_after_reinitialize(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        permit: Option<DomainReinitPermit>,
    ) -> Result<Option<DomainResumed>, ControlIoResumeError> {
        match self {
            Self::None => {
                if let Some(permit) = permit {
                    return Err(ControlIoResumeError::UnexpectedPermit { _permit: permit });
                }
                Ok(None)
            }
            Self::Split(shared) => {
                let permit = permit.ok_or(ControlIoResumeError::MissingPermit)?;
                let resumed = shared
                    .domain
                    .resume_after_reinitialize(permit)
                    .map_err(ControlIoResumeError::Driver)?;
                if let Err(error) = shared.requests.resume_after_reinitialize() {
                    return Err(ControlIoResumeError::RequestLifecycle {
                        error,
                        _resumed: resumed,
                    });
                }
                Ok(Some(resumed))
            }
            Self::Combined(requests) => {
                let permit = permit.ok_or(ControlIoResumeError::MissingPermit)?;
                let resumed = published
                    .published_mut()
                    .resume_shared_io_after_reinitialize(permit)
                    .map_err(ControlIoResumeError::Driver)?;
                if let Err(error) = requests.resume_after_reinitialize() {
                    return Err(ControlIoResumeError::RequestLifecycle {
                        error,
                        _resumed: resumed,
                    });
                }
                Ok(Some(resumed))
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ControlIoReclaimError {
    #[error("combined control/I/O owner is unavailable after publication")]
    CombinedUnavailable,
    #[error(transparent)]
    Recovery(DomainRecoveryReclaimError),
    #[error(transparent)]
    Close(DomainTerminalCloseError),
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ControlIoResumeError {
    #[error("controller reinitialization did not provide the control I/O domain permit")]
    MissingPermit,
    #[error("controller reinitialization produced an I/O permit for a control-only owner")]
    UnexpectedPermit { _permit: DomainReinitPermit },
    #[error(transparent)]
    Driver(DomainResumeFailure),
    #[error("request gates failed to resume after the portable I/O domain: {error}")]
    RequestLifecycle {
        error: DomainRequestLifecycleError,
        _resumed: DomainResumed,
    },
}

enum ControlIoDispatchFailure {
    Driver(DomainRequestServiceError),
    CombinedUnavailable,
}

fn dispatch_control_io(
    published: &mut RdifBlockPublishedOwner,
    control_io: &mut ControlIoRuntime,
    budget: usize,
) -> Result<usize, ControlIoDispatchFailure> {
    match control_io {
        ControlIoRuntime::None => Ok(0),
        ControlIoRuntime::Split(shared) => shared
            .requests
            .dispatch(shared.domain.io_mut(), budget)
            .map_err(ControlIoDispatchFailure::Driver),
        ControlIoRuntime::Combined(requests) => {
            let Some(mut domain) = published.published_mut().shared_io_domain_mut() else {
                return Err(ControlIoDispatchFailure::CombinedUnavailable);
            };
            requests
                .dispatch(&mut domain, budget)
                .map_err(ControlIoDispatchFailure::Driver)
        }
    }
}

pub(super) fn install_request_runtime(
    published: &RdifBlockPublishedOwner,
    installation: &FinalDomainInstallation,
    control_remote: &DeviceMaintenanceHandle<V13MaintenanceEvent>,
    config: crate::block::BlockRuntimeConfig,
) -> Result<RequestRuntimeInstallation, RequestRuntimeInstallError> {
    let mut endpoints = Vec::new();
    let mut queue_descs = Vec::new();
    let mut combined_requests = None;
    if let Some(shared) = installation.shared_domain.as_ref() {
        let runtime = Arc::clone(shared.requests.runtime());
        queue_descs.extend_from_slice(runtime.queue_descs());
        let remote = control_remote
            .try_clone_task_context()
            .map_err(RequestRuntimeInstallError::Maintenance)?;
        endpoints.push(Arc::new(DomainSubmitEndpoint::new(runtime, remote)));
    }
    if let Some(queues) = published.published().shared_io_queues() {
        if installation.shared_domain.is_some() {
            return Err(RequestRuntimeInstallError::ConflictingSharedOwner);
        }
        let domain = queues
            .first()
            .map(rdif_block::InterruptQueueDesc::ownership_domain)
            .ok_or(RequestRuntimeInstallError::Build(
                RequestRuntimeBuildError::InvalidDomainTopology,
            ))?;
        let runtime = Arc::new(
            DomainRequestRuntime::new(domain, queues, config)
                .map_err(RequestRuntimeInstallError::Build)?,
        );
        queue_descs.extend_from_slice(runtime.queue_descs());
        let remote = control_remote
            .try_clone_task_context()
            .map_err(RequestRuntimeInstallError::Maintenance)?;
        endpoints.push(Arc::new(DomainSubmitEndpoint::new(
            Arc::clone(&runtime),
            remote,
        )));
        combined_requests = Some(DomainRequestOwner::new(runtime));
    }
    for child in &installation.child_domains {
        queue_descs.extend_from_slice(child.requests.queue_descs());
        let remote = child
            .remote
            .try_clone_task_context()
            .map_err(RequestRuntimeInstallError::Maintenance)?;
        endpoints.push(Arc::new(DomainSubmitEndpoint::new(
            Arc::clone(&child.requests),
            remote,
        )));
    }
    let devices = build_published_devices(
        published.published().logical_devices(),
        published.published().logical_device_routes(),
        &endpoints,
        &queue_descs,
        crate::runtime_cpu_count(),
        config,
    )
    .map_err(RequestRuntimeInstallError::Build)?;
    for child in &installation.child_domains {
        child
            .remote
            .publish_cause(crate::maintenance::MaintenanceCauses::SUBMIT)
            .map_err(RequestRuntimeInstallError::Maintenance)?;
    }
    Ok(RequestRuntimeInstallation {
        devices,
        combined_requests,
    })
}

pub(super) struct ControlServiceOwner {
    pub(super) published: RdifBlockPublishedOwner,
    pub(super) control_io: ControlIoRuntime,
    pub(super) sources: Vec<BoundEvidenceSource>,
    pub(super) session: MaintenanceSession<V13MaintenanceEvent>,
    pub(super) topology: Arc<FixedOwnershipTopology>,
    pub(super) shutdown: Arc<ControllerShutdown>,
    pub(super) participant: ParticipantId,
    pub(super) child_shutdown_remotes: Vec<DeviceMaintenanceHandle<V13MaintenanceEvent>>,
    pub(super) child_reinit_cells: Vec<Arc<DomainReinitPermitCell>>,
}

pub(super) fn service_control_forever(owner: ControlServiceOwner) -> MaintenanceClosed {
    let ControlServiceOwner {
        mut published,
        mut control_io,
        mut sources,
        session,
        topology,
        shutdown,
        participant,
        child_shutdown_remotes,
        child_reinit_cells,
    } = owner;
    let control_source_ids = sources
        .iter()
        .map(BoundEvidenceSource::source)
        .collect::<Vec<_>>();
    let mut shutdown = ControlLifecycle::new(
        shutdown,
        participant,
        child_shutdown_remotes,
        child_reinit_cells,
        control_source_ids,
    );
    loop {
        let mut recovery_fault = None;
        let drain = match session.drain_owner(OWNER_SERVICE_BUDGET, |event| {
            if let V13MaintenanceEvent::Recovery { fault } = event {
                recovery_fault.get_or_insert(fault);
            }
        }) {
            Ok(drain) => drain,
            Err(error) => quarantine_control_owner(
                session,
                "drain controller maintenance mailbox",
                shutdown,
                (published, control_io, sources, error, topology),
            ),
        };
        if drain.causes().contains(MaintenanceCauses::SHUTDOWN)
            && let Err(error) = shutdown.request_close(&control_io)
        {
            quarantine_control_owner(
                session,
                "begin controller shutdown transaction",
                shutdown,
                (published, control_io, sources, error, topology),
            );
        }
        if let Some(fault) = recovery_fault
            && let Err(error) = shutdown.request_recovery(&control_io, fault)
        {
            quarantine_control_owner(
                session,
                "begin controller recovery requested by an I/O owner",
                shutdown,
                (published, control_io, sources, error, topology),
            );
        }
        // Resume the local driver/request gates before consuming IRQs that may
        // arrive immediately after the controller publishes its new epoch.
        if shutdown.phase() == ShutdownPhase::OwnersResuming {
            match shutdown.advance(&mut published, &mut control_io, &mut sources) {
                Ok(ControlLifecycleAdvance::Pending) => {}
                Ok(ControlLifecycleAdvance::Closed) => {
                    return close_control_session(
                        session, published, control_io, sources, shutdown, topology,
                    );
                }
                Err(error) => quarantine_control_owner(
                    session,
                    "resume controller owner before IRQ service",
                    shutdown,
                    (published, control_io, sources, error, topology),
                ),
            }
        }
        let mut fault_budget = 0;
        while fault_budget < OWNER_SERVICE_BUDGET {
            let Some((source_index, fault)) =
                take_first_owner(&mut sources, |source| source.take_fault())
            else {
                break;
            };
            if let Err(error) = shutdown.accept_contained_source_fault(
                &published,
                &control_io,
                &mut sources,
                source_index,
                fault,
            ) {
                quarantine_control_owner(
                    session,
                    "suspend contained controller IRQ source fault",
                    shutdown,
                    (published, control_io, sources, error, topology),
                );
            }
            fault_budget += 1;
        }
        let mut serviced = 0;
        while serviced < OWNER_SERVICE_BUDGET {
            let Some((source_index, pending)) =
                take_first_owner(&mut sources, BoundEvidenceSource::take_pending)
            else {
                break;
            };
            if shutdown.phase() == ShutdownPhase::ControllerReinitializing {
                if let Err(error) = shutdown.service_reinitialize_irq(
                    &mut published,
                    &mut sources[source_index],
                    pending,
                ) {
                    quarantine_control_owner(
                        session,
                        "service controller reinitialization IRQ evidence",
                        shutdown,
                        (published, control_io, sources, error, topology),
                    );
                }
                serviced += 1;
                continue;
            }
            let (decision, completed, route) =
                match service_ready_evidence(&mut published, &mut control_io, pending) {
                    Ok(decision) => decision,
                    Err(failure) => quarantine_control_owner(
                        session,
                        failure.phase,
                        shutdown,
                        (published, control_io, sources, failure.retained, topology),
                    ),
                };
            let controller_identity = published.published().control().controller_identity();
            let applied = match route {
                ReadyEvidenceRoute::SplitIo => apply_domain_decision(
                    &mut sources[source_index],
                    decision,
                    controller_identity,
                    crate::block::activation_v13::source::recovery::DriverEvidenceRoute::Io,
                    |evidence| match &mut control_io {
                        ControlIoRuntime::Split(shared) => shared
                            .domain
                            .io_mut()
                            .commit_drained_evidence(evidence),
                        _ => Err(rdif_block::BlkError::Other(
                            "split I/O evidence route changed before driver retirement",
                        )),
                    },
                ),
                ReadyEvidenceRoute::CombinedIo => apply_domain_decision(
                    &mut sources[source_index],
                    decision,
                    controller_identity,
                    crate::block::activation_v13::source::recovery::DriverEvidenceRoute::Io,
                    |evidence| {
                        let Some(mut domain) =
                            published.published_mut().shared_io_domain_mut()
                        else {
                            return Err(rdif_block::BlkError::Other(
                                "combined I/O owner unavailable during evidence retirement",
                            ));
                        };
                        domain.commit_drained_evidence(evidence)
                    },
                ),
                ReadyEvidenceRoute::Control => apply_domain_decision(
                    &mut sources[source_index],
                    decision,
                    controller_identity,
                    crate::block::activation_v13::source::recovery::DriverEvidenceRoute::Control,
                    |evidence| {
                        published
                            .published_mut()
                            .control_mut()
                            .commit_drained_evidence(evidence)
                    },
                ),
            };
            match applied {
                Ok(DomainDecisionApplied::RecoveryRequired(fault)) => {
                    if let Err(error) = shutdown.request_recovery(&control_io, fault) {
                        quarantine_control_owner(
                            session,
                            "begin controller recovery from IRQ evidence",
                            shutdown,
                            (published, control_io, sources, error, topology),
                        );
                    }
                }
                Ok(
                    DomainDecisionApplied::EvidenceRetained
                    | DomainDecisionApplied::EvidenceDrained,
                ) => {}
                Err(failure) => quarantine_control_owner(
                    session,
                    "apply ready IRQ evidence disposition",
                    shutdown,
                    (published, control_io, sources, failure, topology),
                ),
            }
            serviced = serviced.saturating_add(completed.max(1));
        }
        if serviced < OWNER_SERVICE_BUDGET && shutdown.phase() == ShutdownPhase::Running {
            let dispatch_budget = OWNER_SERVICE_BUDGET - serviced;
            let dispatched =
                match dispatch_control_io(&mut published, &mut control_io, dispatch_budget) {
                    Ok(dispatched) => dispatched,
                    Err(ControlIoDispatchFailure::Driver(error)) => quarantine_control_owner(
                        session,
                        "dispatch shared-domain staged request",
                        shutdown,
                        (published, control_io, sources, error, topology),
                    ),
                    Err(ControlIoDispatchFailure::CombinedUnavailable) => quarantine_control_owner(
                        session,
                        "borrow combined shared I/O owner for dispatch",
                        shutdown,
                        (published, control_io, sources, topology),
                    ),
                };
            serviced += dispatched;
        }
        let phase_before = shutdown.phase();
        match shutdown.advance(&mut published, &mut control_io, &mut sources) {
            Ok(ControlLifecycleAdvance::Pending) => {}
            Ok(ControlLifecycleAdvance::Closed) => {
                return close_control_session(
                    session, published, control_io, sources, shutdown, topology,
                );
            }
            Err(error) => quarantine_control_owner(
                session,
                "advance controller shutdown transaction",
                shutdown,
                (published, control_io, sources, error, topology),
            ),
        }
        let phase_after = shutdown.phase();
        let shutdown_progressed = phase_before != phase_after;
        if serviced >= OWNER_SERVICE_BUDGET
            || drain.pending()
            || (phase_after == ShutdownPhase::Running && control_io.has_staged())
            || shutdown_progressed
            || shutdown.requires_immediate_progress()
        {
            let _ = crate::task::yield_current_cpu();
            continue;
        }
        let request_deadline = (shutdown.phase() == ShutdownPhase::Running)
            .then(|| control_io.earliest_deadline())
            .flatten();
        let deadline = earliest_deadline(request_deadline, shutdown.wait_deadline());
        if let Some(deadline) = deadline {
            match session.wait_for_pending_until(deadline) {
                Ok(crate::maintenance::MaintenanceWaitOutcome::ConditionMet) => {}
                Ok(crate::maintenance::MaintenanceWaitOutcome::TimedOut)
                    if shutdown.phase() == ShutdownPhase::Running
                        && control_io.has_expired(ax_hal::time::monotonic_time_nanos()) =>
                {
                    if let Err(error) = shutdown
                        .request_recovery(&control_io, rdif_block::ControllerFault::LostIrqEvidence)
                    {
                        quarantine_control_owner(
                            session,
                            "begin shared-domain watchdog recovery",
                            shutdown,
                            (published, control_io, sources, error, topology),
                        );
                    }
                }
                Ok(crate::maintenance::MaintenanceWaitOutcome::TimedOut) => {}
                Err(error) => quarantine_control_owner(
                    session,
                    "wait for shared-domain deadline or evidence",
                    shutdown,
                    (published, control_io, sources, error, topology),
                ),
            }
        } else if let Err(error) = session.wait_for_pending() {
            quarantine_control_owner(
                session,
                "wait for controller evidence",
                shutdown,
                (published, control_io, sources, error, topology),
            );
        }
    }
}

fn earliest_deadline(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}
