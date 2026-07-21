//! rdif-block v0.13 activation boundary for one AHCI ownership domain.

use alloc::{boxed::Box, format, sync::Arc, vec, vec::Vec};
use core::{any::Any, num::NonZeroU16};

use rdif_block::{
    ActivationError, ActivationFailure, ActivationPlan, BlkError, ControlProgress, ControlSchedule,
    ControllerActivator, ControllerCapabilities, ControllerControl, ControllerControlPart,
    ControllerFault, ControllerPublicationFactory, ControllerReinitialized, DomainIrqSource,
    DriverControlPoll, DriverControlTrigger, DriverEvidenceRetirement, DriverGeneric,
    DriverLogicalDeviceDesc, DriverPrepareErrorCode, EvidenceServiceResult, HardwareQueueDepth,
    IdList, InitError, InitInput, InitPoll, InterruptLifecycle, InterruptQueueDesc,
    IoDomainBuildFailure, IoDomainIrqSource, IoDomainPart, IrqEvidenceId, IrqSourceId,
    LifecycleEndpoint, LogicalDeviceConstraints, LogicalDeviceSelector, OwnershipDomainCapability,
    OwnershipDomainId, OwnershipDomainIds, PreparedControllerParts, PublicationBuildFailure,
    QueueExecution, RecoveryCause, RecoveryEvidenceRetireFailure, RecoveryEvidenceRetirePermit,
    RecoveryEvidenceRetired,
};

use crate::{
    controller::AhciHost,
    domain_v13::{
        AhciDomainPort, AhciDomainRecoveryEpoch, AhciV13IoDomain, driver_key_for_port,
        evidence_result,
    },
    evidence::AhciEvidenceLedger,
    irq::HostShared,
    queue::{AhciPortQueue, ReadyPort},
    registers::MAX_PORTS,
};

/// Discovered AHCI controller waiting for one immutable runtime plan.
pub struct AhciControllerActivator {
    host: AhciHost,
    capabilities: ControllerCapabilities,
    domain: OwnershipDomainId,
}

impl AhciControllerActivator {
    pub(crate) fn new(host: AhciHost) -> Result<Self, ActivationError> {
        let domain = OwnershipDomainId::new(0)?;
        let implemented_count = u16::try_from(host.v13_implemented_ports().count_ones())
            .ok()
            .and_then(NonZeroU16::new)
            .ok_or(ActivationError::MissingOwnershipDomains)?;
        let mut sources = IdList::none();
        sources.insert(host.v13_irq_source_id());
        let domains = vec![OwnershipDomainCapability::new(
            domain,
            LogicalDeviceSelector::AllPublished,
            QueueExecution::Serialized,
            implemented_count,
            implemented_count,
            HardwareQueueDepth::fixed(NonZeroU16::MIN),
            sources,
        )?];
        let capabilities = ControllerCapabilities::new_discovering(
            host.v13_controller_identity(),
            rdif_block::ControlDomainCapability::shared_with_io(domain, sources)?,
            implemented_count,
            LogicalDeviceConstraints::discover_during_init(
                host.v13_dma_domain(),
                host.v13_dma_mask(),
            ),
            OwnershipDomainIds::from_bits(1_u64 << domain.get()),
            domains,
        )?;
        Ok(Self {
            host,
            capabilities,
            domain,
        })
    }
}

impl DriverGeneric for AhciControllerActivator {
    fn name(&self) -> &str {
        self.host.v13_name()
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl ControllerActivator for AhciControllerActivator {
    fn capabilities(&self) -> &ControllerCapabilities {
        &self.capabilities
    }

    fn activate(
        self: Box<Self>,
        plan: ActivationPlan,
    ) -> Result<PreparedControllerParts, ActivationFailure> {
        let Some(selected) = plan.domain(self.domain) else {
            return Err(ActivationFailure::new(
                ActivationError::MissingDomainPlan {
                    domain: self.domain,
                },
                self,
            ));
        };
        if plan.controller_identity() != self.capabilities.controller_identity()
            || plan.domains().len() != 1
            || plan.control_domain() != self.domain
        {
            return Err(ActivationFailure::new(
                ActivationError::ControllerIdentityMismatch,
                self,
            ));
        }
        let selected_ports = implemented_port_ids(self.host.v13_implemented_ports());
        if selected_ports.len() != usize::from(selected.queue_count().get())
            || selected.queue_depth() != NonZeroU16::MIN
        {
            return Err(ActivationFailure::new(
                ActivationError::DriverPreparationFailed {
                    code: DriverPrepareErrorCode::UnsupportedTopology,
                },
                self,
            ));
        }
        let source_id = match IrqSourceId::new(self.host.v13_irq_source_id()) {
            Ok(source) if selected.irq_sources().contains(source.get()) => source,
            _ => {
                return Err(ActivationFailure::new(
                    ActivationError::InvalidIrqSelection {
                        domain: self.domain,
                    },
                    self,
                ));
            }
        };
        let Some((source, ledger)) = self.host.v13_shared().take_v13_source(source_id) else {
            return Err(ActivationFailure::new(
                ActivationError::DriverPreparationFailed {
                    code: DriverPrepareErrorCode::ResourceUnavailable,
                },
                self,
            ));
        };
        let AhciControllerActivator {
            host,
            capabilities: _,
            domain,
        } = *self;
        let control = AhciV13Control::new(
            host,
            ledger,
            domain,
            source_id,
            selected_ports,
            selected.queue_depth(),
        );
        let control_part = match ControllerControlPart::new_shared(
            domain,
            vec![DomainIrqSource::new(source_id, source)],
            Box::new(control),
        ) {
            Ok(control) => control,
            Err(failure) => return Err(ActivationFailure::control_part(plan, failure)),
        };
        PreparedControllerParts::new(plan, control_part).map_err(ActivationFailure::prepared)
    }
}

fn implemented_port_ids(bitmap: u32) -> Vec<usize> {
    (0..MAX_PORTS)
        .filter(|port| bitmap & (1_u32 << port) != 0)
        .collect()
}

struct AhciV13Control {
    host: AhciHost,
    ledger: Arc<AhciEvidenceLedger>,
    domain: OwnershipDomainId,
    source: IrqSourceId,
    selected_ports: Vec<usize>,
    queue_depth: NonZeroU16,
    publication_failure: Option<AhciPublicationFailure>,
    published: bool,
    lifecycle_operation: Option<AhciLifecycleOperation>,
    recovery_epoch: Arc<AhciDomainRecoveryEpoch>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AhciLifecycleOperation {
    Quiescing,
    Reinitializing,
}

enum AhciPublicationFailure {
    Descriptor {
        error: ActivationError,
        ports: Vec<AhciDomainPort>,
        ready: Vec<ReadyPort>,
    },
    UnexpectedReadyPort {
        ports: Vec<AhciDomainPort>,
        ready: Vec<ReadyPort>,
    },
    IoDomain(IoDomainBuildFailure),
    Publication(PublicationBuildFailure),
}

impl AhciPublicationFailure {
    fn init_error(&self) -> InitError {
        match self {
            Self::Descriptor {
                error,
                ports,
                ready,
            } => {
                let _ = (error, ports.len(), ready.len());
            }
            Self::UnexpectedReadyPort { ports, ready } => {
                let _ = (ports.len(), ready.len());
            }
            Self::IoDomain(failure) => {
                let _ = failure.error();
            }
            Self::Publication(failure) => {
                let _ = failure.error();
            }
        }
        InitError::Hardware("AHCI Ready publication is quarantined")
    }
}

impl AhciV13Control {
    fn new(
        host: AhciHost,
        ledger: Arc<AhciEvidenceLedger>,
        domain: OwnershipDomainId,
        source: IrqSourceId,
        selected_ports: Vec<usize>,
        queue_depth: NonZeroU16,
    ) -> Self {
        Self {
            host,
            ledger,
            domain,
            source,
            selected_ports,
            queue_depth,
            publication_failure: None,
            published: false,
            lifecycle_operation: None,
            recovery_epoch: Arc::new(AhciDomainRecoveryEpoch::new()),
        }
    }

    fn service_lifecycle(&mut self, now_ns: u64) -> DriverControlPoll {
        let progress = match self.lifecycle_operation {
            Some(AhciLifecycleOperation::Quiescing) => {
                let poll = self.host.v13_poll_dma_quiesce(InitInput::at(now_ns));
                self.finish_quiesce_poll(poll)
            }
            Some(AhciLifecycleOperation::Reinitializing) => {
                let poll = self.host.v13_poll_reinitialize(InitInput::at(now_ns));
                self.finish_reinitialize_poll(poll)
            }
            None => ControlProgress::Failed(InitError::InvalidState),
        };
        DriverControlPoll::without_evidence(progress)
    }

    fn begin_quiesce(
        &mut self,
        now_ns: u64,
        epoch: rdif_block::ControllerEpoch,
    ) -> DriverControlPoll {
        if !self.published || self.lifecycle_operation.is_some() {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(
                InitError::InvalidState,
            ));
        }
        if let Err(error) = self.host.v13_begin_dma_quiesce(epoch) {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(error));
        }
        self.lifecycle_operation = Some(AhciLifecycleOperation::Quiescing);
        self.service_lifecycle(now_ns)
    }

    fn begin_reinitialize(
        &mut self,
        now_ns: u64,
        quiesced: rdif_block::DmaQuiesced,
    ) -> DriverControlPoll {
        if !self.published
            || self.lifecycle_operation.is_some()
            || !self.recovery_epoch.matches(quiesced.epoch())
        {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(
                InitError::InvalidState,
            ));
        }
        if let Err(error) = self.host.v13_begin_reinitialize(quiesced) {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(error));
        }
        self.lifecycle_operation = Some(AhciLifecycleOperation::Reinitializing);
        self.service_lifecycle(now_ns)
    }

    fn finish_quiesce_poll(&mut self, poll: InitPoll<rdif_block::DmaQuiesced>) -> ControlProgress {
        match poll {
            InitPoll::Ready(proof) => {
                self.lifecycle_operation = None;
                ControlProgress::DmaQuiesced(proof)
            }
            InitPoll::Pending(schedule) => finish_pending_schedule(schedule),
            InitPoll::Failed(error) => {
                self.lifecycle_operation = None;
                ControlProgress::Failed(error)
            }
        }
    }

    fn finish_reinitialize_poll(
        &mut self,
        poll: InitPoll<rdif_block::ControllerReady>,
    ) -> ControlProgress {
        match poll {
            InitPoll::Ready(ready) => {
                self.lifecycle_operation = None;
                match ControllerReinitialized::new(ready, vec![self.domain]) {
                    Ok(reinitialized) => ControlProgress::Reinitialized(reinitialized),
                    Err(_) => ControlProgress::Failed(InitError::Hardware(
                        "AHCI reinitialization domain proof set is invalid",
                    )),
                }
            }
            InitPoll::Pending(schedule) => finish_pending_schedule(schedule),
            InitPoll::Failed(error) => {
                self.lifecycle_operation = None;
                ControlProgress::Failed(error)
            }
        }
    }

    fn service_without_irq(
        &mut self,
        now_ns: u64,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        if self.published {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(
                InitError::InvalidState,
            ));
        }
        let poll = self.host.v13_poll_init(now_ns, false);
        DriverControlPoll::without_evidence(self.finish_init_poll(poll, publication))
    }

    fn service_irq(
        &mut self,
        now_ns: u64,
        evidence: IrqEvidenceId,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        if self.published {
            return DriverControlPoll::after_irq(
                ControlProgress::Failed(InitError::InvalidState),
                EvidenceServiceResult::Recover(ControllerFault::Ownership),
            );
        }
        let batch = match self.ledger.begin_service(evidence) {
            Ok(batch) => batch,
            Err(_) => {
                return DriverControlPoll::after_irq(
                    ControlProgress::Failed(InitError::InvalidState),
                    EvidenceServiceResult::Recover(ControllerFault::Ownership),
                );
            }
        };
        let port_facts = batch.port_facts();
        let poll = self.host.v13_poll_init(now_ns, true);
        let retained = match retained_initialization_facts(self.host.v13_shared(), port_facts) {
            Ok(retained) => retained,
            Err(fault) => {
                return DriverControlPoll::after_irq(
                    ControlProgress::Failed(InitError::Hardware(
                        "AHCI initialization IRQ evidence is inconsistent",
                    )),
                    EvidenceServiceResult::Recover(fault),
                );
            }
        };
        let disposition = self.ledger.finish_service(batch, retained);
        let evidence_result = evidence_result(disposition);
        if matches!(poll, InitPoll::Ready(_))
            && !matches!(evidence_result, EvidenceServiceResult::Drained)
        {
            return DriverControlPoll::after_irq(
                ControlProgress::Failed(InitError::Hardware(
                    "AHCI initialized with retained IRQ evidence",
                )),
                EvidenceServiceResult::Recover(ControllerFault::Protocol),
            );
        }
        DriverControlPoll::after_irq(self.finish_init_poll(poll, publication), evidence_result)
    }

    fn finish_init_poll(
        &mut self,
        poll: InitPoll<()>,
        publication: &ControllerPublicationFactory<'_>,
    ) -> ControlProgress {
        match poll {
            InitPoll::Ready(()) => self.publish_ready(publication),
            InitPoll::Pending(schedule) => match ControlSchedule::new(
                schedule.run_again(),
                schedule.irq_sources(),
                schedule.wake_at_ns(),
            ) {
                Ok(schedule) => ControlProgress::Pending(schedule),
                Err(error) => ControlProgress::Failed(error),
            },
            InitPoll::Failed(error) => ControlProgress::Failed(error),
        }
    }

    fn publish_ready(&mut self, publication: &ControllerPublicationFactory<'_>) -> ControlProgress {
        if let Some(failure) = &self.publication_failure {
            return ControlProgress::Failed(failure.init_error());
        }
        if self.host.v13_implemented_ports()
            != self
                .selected_ports
                .iter()
                .fold(0_u32, |bits, port| bits | (1_u32 << port))
        {
            return ControlProgress::Failed(InitError::Hardware(
                "AHCI implemented-port topology changed during activation",
            ));
        }

        let binding = self.host.v13_queue_binding();
        let shared = Arc::clone(self.host.v13_shared());
        let mut ready_ports = self.host.v13_take_ready_ports();
        let mut logical_devices = Vec::with_capacity(ready_ports.len());
        let mut queue_descriptors = Vec::with_capacity(self.selected_ports.len());
        let mut domain_ports = Vec::with_capacity(self.selected_ports.len());
        let mut queue_sources = IdList::none();
        queue_sources.insert(self.source.get());

        for &port in &self.selected_ports {
            let ready = ready_ports
                .iter()
                .position(|ready| ready.port == port)
                .map(|index| ready_ports.swap_remove(index));
            let (selector, queue) = if let Some(ready) = ready {
                let driver_key = driver_key_for_port(port);
                let info = ready.v13_queue_info(binding);
                logical_devices.push(DriverLogicalDeviceDesc::new(
                    driver_key,
                    format!("{}-port{port}", self.host.v13_name()),
                    info.device,
                    info.limits,
                ));
                (
                    LogicalDeviceSelector::Exact(vec![driver_key]),
                    Some(AhciPortQueue::new_v13(ready, Arc::clone(&shared), binding)),
                )
            } else {
                (LogicalDeviceSelector::Unrouted, None)
            };
            let descriptor = match InterruptQueueDesc::new(
                port,
                selector,
                self.domain,
                QueueExecution::Serialized,
                self.queue_depth,
                queue_sources,
            ) {
                Ok(descriptor) => descriptor,
                Err(error) => {
                    domain_ports.push(AhciDomainPort::new(port, queue));
                    let failure = AhciPublicationFailure::Descriptor {
                        error,
                        ports: domain_ports,
                        ready: ready_ports,
                    };
                    let error = failure.init_error();
                    self.publication_failure = Some(failure);
                    return ControlProgress::Failed(error);
                }
            };
            queue_descriptors.push(descriptor);
            domain_ports.push(AhciDomainPort::new(port, queue));
        }

        if !ready_ports.is_empty() {
            let failure = AhciPublicationFailure::UnexpectedReadyPort {
                ports: domain_ports,
                ready: ready_ports,
            };
            let error = failure.init_error();
            self.publication_failure = Some(failure);
            return ControlProgress::Failed(error);
        }
        let domain = AhciV13IoDomain::new(
            self.domain,
            Arc::clone(&self.ledger),
            shared,
            domain_ports,
            self.host.v13_controller_identity(),
            Arc::clone(&self.recovery_epoch),
        );
        let io_domain = match IoDomainPart::new(
            self.domain,
            queue_descriptors,
            vec![IoDomainIrqSource::AlreadyBound(self.source)],
            Box::new(domain),
        ) {
            Ok(domain) => domain,
            Err(failure) => {
                let failure = AhciPublicationFailure::IoDomain(failure);
                let error = failure.init_error();
                self.publication_failure = Some(failure);
                return ControlProgress::Failed(error);
            }
        };
        match publication.publish(logical_devices, vec![io_domain]) {
            Ok(ready) => {
                self.published = true;
                ControlProgress::PublicationReady(ready)
            }
            Err(failure) => {
                let failure = AhciPublicationFailure::Publication(failure);
                let error = failure.init_error();
                self.publication_failure = Some(failure);
                ControlProgress::Failed(error)
            }
        }
    }
}

impl DriverGeneric for AhciV13Control {
    fn name(&self) -> &str {
        self.host.v13_name()
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl ControllerControl for AhciV13Control {
    fn controller_identity(&self) -> core::num::NonZeroUsize {
        self.host.v13_controller_identity()
    }

    fn service_control(
        &mut self,
        trigger: DriverControlTrigger,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        if let Some(failure) = &self.publication_failure {
            let progress = ControlProgress::Failed(failure.init_error());
            return if matches!(trigger, DriverControlTrigger::Irq { .. }) {
                DriverControlPoll::after_irq(
                    progress,
                    EvidenceServiceResult::Recover(ControllerFault::Ownership),
                )
            } else {
                DriverControlPoll::without_evidence(progress)
            };
        }
        match trigger {
            DriverControlTrigger::Start { now_ns } => self.service_without_irq(now_ns, publication),
            DriverControlTrigger::InternalProgress { now_ns }
            | DriverControlTrigger::ProtocolDeadline { now_ns } => {
                if self.published {
                    self.service_lifecycle(now_ns)
                } else {
                    self.service_without_irq(now_ns, publication)
                }
            }
            DriverControlTrigger::Irq { now_ns, evidence } => {
                self.service_irq(now_ns, evidence, publication)
            }
            DriverControlTrigger::BeginQuiesce {
                now_ns,
                intent: _,
                epoch,
            } => self.begin_quiesce(now_ns, epoch),
            DriverControlTrigger::BeginReinitialize { now_ns, quiesced } => {
                self.begin_reinitialize(now_ns, quiesced)
            }
        }
    }

    fn service_ready_evidence(
        &mut self,
        _evidence: IrqEvidenceId,
    ) -> Result<EvidenceServiceResult, BlkError> {
        Ok(EvidenceServiceResult::Recover(ControllerFault::Ownership))
    }

    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        self.ledger
            .commit_drained_evidence(evidence)
            .map_err(|_| BlkError::Other("AHCI control evidence commit is invalid"))
    }

    fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        self.ledger
            .retire_after_quiesce(permit, self.host.v13_controller_identity())
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        LifecycleEndpoint::Interrupt(self)
    }

    fn enable_irq(&mut self) -> Result<(), BlkError> {
        self.host.enable_irq()
    }

    fn disable_irq(&mut self) -> Result<(), BlkError> {
        self.host.disable_irq()
    }

    fn is_irq_enabled(&self) -> bool {
        self.host.is_irq_enabled()
    }
}

impl InterruptLifecycle for AhciV13Control {
    fn controller_cookie(&self) -> usize {
        self.host.v13_controller_identity().get()
    }

    fn begin_dma_quiesce(
        &mut self,
        epoch: rdif_block::ControllerEpoch,
        _cause: RecoveryCause,
    ) -> Result<(), InitError> {
        if self.lifecycle_operation.is_some() {
            return Err(InitError::InvalidState);
        }
        self.host.v13_begin_dma_quiesce(epoch)?;
        self.lifecycle_operation = Some(AhciLifecycleOperation::Quiescing);
        Ok(())
    }

    fn poll_dma_quiesce(&mut self, input: InitInput) -> InitPoll<rdif_block::DmaQuiesced> {
        if self.lifecycle_operation != Some(AhciLifecycleOperation::Quiescing) {
            return InitPoll::Failed(InitError::InvalidState);
        }
        let poll = self.host.v13_poll_dma_quiesce(input);
        if matches!(poll, InitPoll::Ready(_) | InitPoll::Failed(_)) {
            self.lifecycle_operation = None;
        }
        poll
    }

    fn enter_guest_owned(&mut self, quiesced: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        if self.lifecycle_operation.is_some() || !self.recovery_epoch.matches(quiesced.epoch()) {
            return Err(InitError::InvalidState);
        }
        self.host.v13_enter_guest_owned(quiesced)
    }

    fn begin_reinitialize(&mut self, quiesced: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        if self.lifecycle_operation.is_some() || !self.recovery_epoch.matches(quiesced.epoch()) {
            return Err(InitError::InvalidState);
        }
        self.host.v13_begin_reinitialize(quiesced)?;
        self.lifecycle_operation = Some(AhciLifecycleOperation::Reinitializing);
        Ok(())
    }

    fn poll_reinitialize(&mut self, input: InitInput) -> InitPoll<rdif_block::ControllerReady> {
        if self.lifecycle_operation != Some(AhciLifecycleOperation::Reinitializing) {
            return InitPoll::Failed(InitError::InvalidState);
        }
        let poll = self.host.v13_poll_reinitialize(input);
        if matches!(poll, InitPoll::Ready(_) | InitPoll::Failed(_)) {
            self.lifecycle_operation = None;
        }
        poll
    }
}

fn finish_pending_schedule(schedule: rdif_block::InitSchedule) -> ControlProgress {
    match ControlSchedule::new(
        schedule.run_again(),
        schedule.irq_sources(),
        schedule.wake_at_ns(),
    ) {
        Ok(schedule) => ControlProgress::Pending(schedule),
        Err(error) => ControlProgress::Failed(error),
    }
}

fn retained_initialization_facts(
    shared: &HostShared,
    port_facts: u32,
) -> Result<u32, ControllerFault> {
    let mut retained = 0_u32;
    for port in 0..MAX_PORTS {
        let bit = 1_u32 << port;
        if port_facts & bit == 0 {
            continue;
        }
        if shared.port(port).take_overflow() {
            return Err(ControllerFault::Protocol);
        }
        if shared.port(port).active_request_generation() == 0 {
            for _ in 0..crate::irq::IRQ_SNAPSHOT_CAPACITY {
                let Some(snapshot) = shared.port(port).pop_snapshot() else {
                    break;
                };
                if snapshot.has_error() {
                    return Err(ControllerFault::Protocol);
                }
            }
        }
        if shared.port(port).has_snapshots() {
            retained |= bit;
        }
    }
    Ok(retained)
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, vec};
    use core::num::NonZeroU64;

    use dma_api::DeviceDma;
    use rdif_block::{
        CompletedRequest, CompletionSink, ControlProgress, ControlTrigger, ControllerEpoch,
        DomainActivationPlan, DomainOwnerBinding, IrqCapture, LogicalDeviceSelector, OwnedRequest,
        QuiesceIntent, RequestFlags, RequestId, RequestOp,
    };

    use super::*;
    use crate::{
        AhciConfig,
        registers::{
            HOST_IS, HOST_PI, IRQ_D2H_REG_FIS, IRQ_TASK_FILE_ERROR, MMIO_REQUIRED_SIZE, PX_CI,
            PX_IS, PX_SERR, PX_TFD, TFD_ERR, port_offset, tests_support::FakeRegisters,
        },
        test_support::TEST_DMA,
    };

    #[test]
    fn mixed_active_and_empty_ports_publish_exact_and_unrouted_queues() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        registers.set(HOST_PI, 0b1011);
        let mut host = AhciHost::from_test_parts(
            "test-ahci",
            registers.shared(),
            DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
            AhciConfig::legacy_irq(2),
        );
        host.install_v13_test_disk(0, 4_096);
        host.install_v13_test_disk(3, 8_192);
        host.mark_v13_ready_for_test();
        let activator = host.into_v13_activator().unwrap();
        let capability = &activator.capabilities().domains()[0];
        let plan = ActivationPlan::new(
            activator.capabilities(),
            vec![DomainActivationPlan::new(
                capability.id(),
                capability.max_queues(),
                capability.queue_depth().max(),
                capability.irq_sources(),
            )],
        )
        .unwrap();

        let mut prepared = Box::new(activator).activate(plan).unwrap();
        let source = prepared.control_mut().owned_irq_sources_mut()[0]
            .take_for_registration()
            .unwrap();
        prepared.control_mut().owned_irq_sources_mut()[0]
            .finish_registration()
            .unwrap();
        prepared.enable_irq().unwrap();
        let poll = prepared
            .service_control(ControlTrigger::Start { now_ns: 0 })
            .unwrap();
        let (ControlProgress::PublicationReady(mut ready), None) = poll.into_parts() else {
            panic!("an already-ready fixture must publish without IRQ evidence")
        };

        assert_eq!(ready.logical_devices().len(), 2);
        let queues = ready.io_domains()[0].queues();
        assert_eq!(queues.len(), 3);
        assert_eq!(
            queues.iter().map(|queue| queue.id()).collect::<Vec<_>>(),
            [0, 1, 3]
        );
        assert!(matches!(
            queues[0].logical_devices(),
            LogicalDeviceSelector::Exact(keys) if keys.len() == 1
        ));
        assert!(matches!(
            queues[1].logical_devices(),
            LogicalDeviceSelector::Unrouted
        ));
        assert!(matches!(
            queues[2].logical_devices(),
            LogicalDeviceSelector::Exact(keys) if keys.len() == 1
        ));

        let (mut endpoint, mut irq_control) = source.into_parts();
        let domain = ready.io_domains_mut()[0].io_mut();
        let request = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        };
        let accepted = domain
            .submit_owned(0, driver_key_for_port(0), RequestId::new(11), request)
            .unwrap();
        assert_eq!(accepted.id(), RequestId::new(11));
        registers.set(port_offset(0, PX_CI), 0);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);
        registers.set(HOST_IS, 1);
        let IrqCapture::Captured { event, masked } = endpoint.capture() else {
            panic!("the completed command must publish one evidence identity")
        };
        let mut completions = TestCompletions::default();
        assert_eq!(
            domain.service_evidence(event, &mut completions).unwrap(),
            EvidenceServiceResult::Drained
        );
        assert_eq!(completions.0.len(), 1);
        assert_eq!(completions.0[0].id, RequestId::new(11));
        irq_control.rearm(masked.unwrap()).unwrap();

        let port_zero = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        };
        let port_three = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        };
        let accepted_zero = domain
            .submit_owned(0, driver_key_for_port(0), RequestId::new(12), port_zero)
            .unwrap();
        let accepted_three = domain
            .submit_owned(3, driver_key_for_port(3), RequestId::new(13), port_three)
            .unwrap();
        assert_eq!(accepted_zero.id(), RequestId::new(12));
        assert_eq!(accepted_three.id(), RequestId::new(13));
        registers.set(port_offset(0, PX_CI), 0);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);
        registers.set(port_offset(3, PX_CI), 0);
        registers.set(port_offset(3, PX_IS), IRQ_D2H_REG_FIS | IRQ_TASK_FILE_ERROR);
        registers.set(port_offset(3, PX_TFD), TFD_ERR);
        registers.set(port_offset(3, PX_SERR), 0x20);
        registers.set(HOST_IS, 0b1001);
        let IrqCapture::Captured { event, .. } = endpoint.capture() else {
            panic!("combined AHCI completion/error status must be captured")
        };
        assert_eq!(
            domain.service_evidence(event, &mut completions).unwrap(),
            EvidenceServiceResult::Recover(ControllerFault::Protocol)
        );
        assert_eq!(
            completions.0.len(),
            1,
            "a later port error in the same evidence must win before success publication"
        );
        assert_eq!(
            domain.service_evidence(event, &mut completions).unwrap(),
            EvidenceServiceResult::Recover(ControllerFault::Protocol),
            "a faulted domain must remain closed until controller reinitialization"
        );
        assert_eq!(completions.0.len(), 1);
    }

    #[test]
    fn control_triggers_drive_quiesce_and_full_reinitialization() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        registers.set(HOST_PI, 1);
        let mut host = AhciHost::from_test_parts(
            "test-ahci",
            registers.shared(),
            DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
            AhciConfig::legacy_irq(2),
        );
        host.install_v13_test_disk(0, 4_096);
        host.v13_shared().port(0).publish_dma_bases(0, 0);
        host.mark_v13_ready_for_test();
        let activator = host.into_v13_activator().unwrap();
        let capability = &activator.capabilities().domains()[0];
        let plan = ActivationPlan::new(
            activator.capabilities(),
            vec![DomainActivationPlan::new(
                capability.id(),
                capability.max_queues(),
                capability.queue_depth().max(),
                capability.irq_sources(),
            )],
        )
        .unwrap();
        let mut prepared = Box::new(activator).activate(plan).unwrap();
        let source = prepared.control_mut().owned_irq_sources_mut()[0]
            .take_for_registration()
            .unwrap();
        prepared.control_mut().owned_irq_sources_mut()[0]
            .finish_registration()
            .unwrap();
        prepared.enable_irq().unwrap();
        let ready = match prepared
            .service_control(ControlTrigger::Start { now_ns: 0 })
            .unwrap()
            .into_parts()
            .0
        {
            ControlProgress::PublicationReady(ready) => ready,
            progress => panic!("ready fixture did not publish: {progress:?}"),
        };
        let staged = prepared.stage(ready).unwrap();
        let (mut coordinator, mut domains) = staged.into_installations();
        let (mut domain, proof) = domains
            .pop()
            .unwrap()
            .finish_binding(DomainOwnerBinding::new(0, NonZeroU64::MIN))
            .unwrap();
        assert!(domains.is_empty());
        coordinator.accept_bound_domain(proof).unwrap();
        let mut published = coordinator.publish().unwrap();

        registers.set(port_offset(0, crate::registers::PX_CMD), 0);
        let progress = published
            .control_mut()
            .service_control(ControlTrigger::BeginQuiesce {
                now_ns: 0,
                intent: QuiesceIntent::Shutdown,
                epoch: ControllerEpoch::new(2),
            })
            .unwrap()
            .into_parts()
            .0;
        assert!(matches!(progress, ControlProgress::Pending(_)));
        let progress = published
            .control_mut()
            .service_control(ControlTrigger::InternalProgress { now_ns: 1 })
            .unwrap()
            .into_parts()
            .0;
        assert!(matches!(progress, ControlProgress::Pending(_)));
        registers.set(crate::registers::HOST_GHC, 0);
        let quiesced = match published
            .control_mut()
            .service_control(ControlTrigger::InternalProgress { now_ns: 2 })
            .unwrap()
            .into_parts()
            .0
        {
            ControlProgress::DmaQuiesced(proof) => proof,
            progress => panic!("controller did not publish DMA proof: {progress:?}"),
        };
        let mut completions = TestCompletions::default();
        domain
            .io_mut()
            .reclaim_after_quiesce(&quiesced, &mut completions)
            .unwrap();

        let progress = published
            .control_mut()
            .service_control(ControlTrigger::BeginReinitialize {
                now_ns: 3,
                quiesced,
            })
            .unwrap()
            .into_parts()
            .0;
        assert!(matches!(progress, ControlProgress::Pending(_)));
        registers.set(crate::registers::HOST_GHC, 0);
        assert!(matches!(
            published
                .control_mut()
                .service_control(ControlTrigger::InternalProgress { now_ns: 4 })
                .unwrap()
                .into_parts()
                .0,
            ControlProgress::Pending(_)
        ));
        assert!(matches!(
            published
                .control_mut()
                .service_control(ControlTrigger::ProtocolDeadline { now_ns: 1_000_004 })
                .unwrap()
                .into_parts()
                .0,
            ControlProgress::Pending(_)
        ));
        registers.set(port_offset(0, crate::registers::PX_SSTS), 3);
        assert!(matches!(
            published
                .control_mut()
                .service_control(ControlTrigger::InternalProgress { now_ns: 1_000_005 })
                .unwrap()
                .into_parts()
                .0,
            ControlProgress::Pending(_)
        ));
        registers.set(
            port_offset(0, crate::registers::PX_CMD),
            crate::registers::CMD_FR,
        );
        assert!(matches!(
            published
                .control_mut()
                .service_control(ControlTrigger::InternalProgress { now_ns: 1_000_006 })
                .unwrap()
                .into_parts()
                .0,
            ControlProgress::Pending(_)
        ));
        registers.set(
            port_offset(0, crate::registers::PX_CMD),
            crate::registers::CMD_CR | crate::registers::CMD_FR,
        );
        registers.set(port_offset(0, PX_TFD), 0);
        let reinitialized = match published
            .control_mut()
            .service_control(ControlTrigger::InternalProgress { now_ns: 1_000_007 })
            .unwrap()
            .into_parts()
            .0
        {
            ControlProgress::Reinitialized(ready) => ready,
            progress => panic!("controller did not publish ready proof: {progress:?}"),
        };
        let reinitialized = published
            .control()
            .bind_reinitialized(reinitialized)
            .unwrap();
        let (mut epoch_commit, mut permits) = reinitialized.into_resume_parts();
        let resumed = domain
            .resume_after_reinitialize(permits.pop().unwrap())
            .unwrap();
        assert!(permits.is_empty());
        epoch_commit.accept_resumed(resumed).unwrap();
        let epoch_commit = epoch_commit.finish().unwrap();
        assert_eq!(
            published.commit_reinitialized_epoch(epoch_commit).unwrap(),
            rdif_block::ControllerEpoch::new(2)
        );

        published.control_mut().enable_irq().unwrap();
        let (mut endpoint, mut irq_control) = source.into_parts();
        let request = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        };
        let accepted = domain
            .io_mut()
            .submit_owned(0, driver_key_for_port(0), RequestId::new(21), request)
            .unwrap();
        assert_eq!(accepted.id(), RequestId::new(21));
        registers.set(port_offset(0, PX_CI), 0);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);
        registers.set(HOST_IS, 1);
        let IrqCapture::Captured { event, masked } = endpoint.capture() else {
            panic!("post-reinitialize completion must publish IRQ evidence")
        };
        domain
            .io_mut()
            .service_evidence(event, &mut completions)
            .unwrap();
        assert_eq!(
            completions.0.last().map(|completion| completion.id),
            Some(RequestId::new(21)),
            "the resumed queue must classify snapshots in the reconstructed epoch"
        );
        irq_control.rearm(masked.unwrap()).unwrap();
    }

    #[derive(Default)]
    struct TestCompletions(Vec<CompletedRequest>);

    impl CompletionSink for TestCompletions {
        fn complete(&mut self, completion: CompletedRequest) {
            self.0.push(completion);
        }
    }
}
