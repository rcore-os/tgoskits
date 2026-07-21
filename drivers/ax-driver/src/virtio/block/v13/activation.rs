//! Two-phase VirtIO block activation and controller ownership.

use alloc::{boxed::Box, sync::Arc, vec};
use core::{
    any::Any,
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
};

use rdif_block::{
    ActivationError, ActivationFailure, ActivationPlan, BlkError, CompletionSink,
    ControlDomainCapability, ControlProgress, ControlSchedule, ControllerActivator,
    ControllerCapabilities, ControllerControl, ControllerControlPart, ControllerEpoch,
    ControllerPublicationFactory, ControllerReady, ControllerReinitialized, DomainIrqSource,
    DriverControlPoll, DriverControlTrigger, DriverDeviceKey, DriverEvidenceRetirement,
    DriverGeneric, DriverLogicalDeviceDesc, EvidenceServiceResult, HardwareQueueDepth, IdList,
    InitError, InitInput, InitPoll, InitSchedule, InterruptIoDomain, InterruptLifecycle,
    InterruptQueueDesc, IrqEvidenceId, IrqSourceId, LifecycleEndpoint, LogicalDeviceConstraints,
    LogicalDeviceSelector, OwnershipDomainCapability, OwnershipDomainId, OwnershipDomainIds,
    PreparedControllerParts, PublicationBuildFailure, QueueExecution, RecoveryCause,
    RecoveryEvidenceRetireFailure, RecoveryEvidenceRetirePermit, RecoveryEvidenceRetired,
    SharedControllerIoDomain, UnacceptedRequest,
};

use super::{
    super::{
        VIRTIO_BLK_IRQ_SOURCE_ID, VIRTIO_BLK_QUEUE_ID,
        device::VirtIoBlkInner,
        initialization::{VIRTIO_BLK_CONFIG_CAPACITY_HIGH, VIRTIO_BLK_CONFIG_CAPACITY_LOW},
        irq::VirtioInterruptPort,
        notify::VirtioQueueNotifyPort,
        queue::VirtioOwnedQueue,
    },
    evidence::{
        VirtioBlockEvidenceFacts, VirtioBlockEvidenceLedger, VirtioEvidenceIrqState,
        new_evidence_source,
    },
    io::VirtioV13IoDomain,
};
use crate::virtio::VirtIoTransport;

/// Discovered VirtIO block controller awaiting an immutable activation plan.
pub struct VirtioBlockActivator<T: VirtIoTransport> {
    name: &'static str,
    capabilities: ControllerCapabilities,
    domain: OwnershipDomainId,
    source: IrqSourceId,
    inner: Box<VirtIoBlkInner<T>>,
    interrupt_port: VirtioInterruptPort,
    notify_port: VirtioQueueNotifyPort,
}

impl<T: VirtIoTransport> VirtioBlockActivator<T> {
    /// Builds a command-free discovery owner.
    ///
    /// The current queue core has one stable descriptor slot, so discovery
    /// advertises exactly one queue and one hardware credit.
    pub fn discovered(
        name: &'static str,
        transport: T,
        interrupt_port: VirtioInterruptPort,
        notify_port: VirtioQueueNotifyPort,
    ) -> Result<Self, ActivationError> {
        let inner = Box::new(VirtIoBlkInner::discovered(transport));
        let identity = NonZeroUsize::new(core::ptr::from_ref(inner.as_ref()).expose_provenance())
            .ok_or(ActivationError::DriverPreparationFailed {
            code: rdif_block::DriverPrepareErrorCode::InvalidState,
        })?;
        let domain = OwnershipDomainId::new(0)?;
        let source = IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID)
            .map_err(|_| ActivationError::InvalidIrqSelection { domain })?;
        let capabilities = virtio_capabilities(identity, domain, source)?;
        Ok(Self {
            name,
            capabilities,
            domain,
            source,
            inner,
            interrupt_port,
            notify_port,
        })
    }
}

impl<T: VirtIoTransport> DriverGeneric for VirtioBlockActivator<T> {
    fn name(&self) -> &str {
        self.name
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl<T: VirtIoTransport> ControllerActivator for VirtioBlockActivator<T> {
    fn capabilities(&self) -> &ControllerCapabilities {
        &self.capabilities
    }

    fn activate(
        self: Box<Self>,
        plan: ActivationPlan,
    ) -> Result<PreparedControllerParts, ActivationFailure> {
        let selected = match plan.domain(self.domain) {
            Some(selected) => selected,
            None => {
                return Err(ActivationFailure::new(
                    ActivationError::MissingDomainPlan {
                        domain: self.domain,
                    },
                    self,
                ));
            }
        };
        if plan.controller_identity() != self.capabilities.controller_identity() {
            return Err(ActivationFailure::new(
                ActivationError::ControllerIdentityMismatch,
                self,
            ));
        }
        if plan.control_domain() != self.domain
            || plan.domains().len() != 1
            || selected.queue_count() != NonZeroU16::MIN
            || selected.queue_depth() != NonZeroU16::MIN
            || selected.irq_sources().bits() != 1_u64 << self.source.get()
        {
            return Err(ActivationFailure::new(
                ActivationError::ControlActivationMismatch,
                self,
            ));
        }
        let mut source_ids = IdList::none();
        source_ids.insert(self.source.get());
        let queue_desc = match InterruptQueueDesc::new(
            VIRTIO_BLK_QUEUE_ID,
            LogicalDeviceSelector::AllPublished,
            self.domain,
            QueueExecution::Tagged,
            NonZeroU16::MIN,
            source_ids,
        ) {
            Ok(queue) => queue,
            Err(error) => return Err(ActivationFailure::new(error, self)),
        };

        let ledger = Arc::new(VirtioBlockEvidenceLedger::new(self.source));
        let irq_state = Arc::new(VirtioEvidenceIrqState::new());
        let Self {
            name,
            capabilities: _,
            domain,
            source,
            inner,
            interrupt_port,
            notify_port,
        } = *self;
        let identity = plan.controller_identity();
        let irq_source =
            new_evidence_source(interrupt_port, Arc::clone(&ledger), Arc::clone(&irq_state));
        let control = VirtioV13Control {
            name,
            identity,
            domain,
            device_key: DriverDeviceKey::new(NonZeroU64::MIN),
            inner,
            notify_port: Some(notify_port),
            ledger,
            irq_state,
            published: false,
            retained_failure: None,
            lifecycle: VirtioV13Lifecycle::Running,
            io: None,
        };
        let control_part = match ControllerControlPart::new_combined_shared(
            domain,
            vec![DomainIrqSource::new(source, irq_source)],
            vec![queue_desc],
            Box::new(control),
        ) {
            Ok(control) => control,
            Err(failure) => return Err(ActivationFailure::control_part(plan, failure)),
        };
        PreparedControllerParts::new(plan, control_part).map_err(ActivationFailure::prepared)
    }
}

struct VirtioV13Control<T: VirtIoTransport> {
    name: &'static str,
    identity: NonZeroUsize,
    domain: OwnershipDomainId,
    device_key: DriverDeviceKey,
    inner: Box<VirtIoBlkInner<T>>,
    notify_port: Option<VirtioQueueNotifyPort>,
    ledger: Arc<VirtioBlockEvidenceLedger>,
    irq_state: Arc<VirtioEvidenceIrqState>,
    published: bool,
    retained_failure: Option<VirtioRetainedFailure>,
    lifecycle: VirtioV13Lifecycle,
    io: Option<VirtioV13IoDomain>,
}

enum VirtioRetainedFailure {
    Publication(PublicationBuildFailure),
    RebuiltQueue { _queue: Box<VirtioOwnedQueue> },
}

impl VirtioRetainedFailure {
    fn init_error(&self) -> InitError {
        match self {
            Self::Publication(failure) => {
                let _ = failure.error();
                InitError::Hardware("VirtIO block publication is quarantined")
            }
            Self::RebuiltQueue { .. } => InitError::Hardware("VirtIO rebuilt queue is quarantined"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VirtioV13Lifecycle {
    Running,
    Resetting {
        epoch: ControllerEpoch,
        deadline_ns: Option<u64>,
    },
    Quiesced {
        epoch: ControllerEpoch,
    },
    Reinitializing {
        epoch: ControllerEpoch,
    },
    GuestOwned,
    Failed,
}

impl<T: VirtIoTransport> VirtioV13Control<T> {
    fn service_init(
        &mut self,
        now_ns: u64,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        let poll = self
            .inner
            .poll_init(InitInput::at(now_ns), self.irq_state.is_enabled());
        DriverControlPoll::without_evidence(self.finish_init_poll(poll, publication))
    }

    fn service_init_irq(
        &mut self,
        now_ns: u64,
        evidence: IrqEvidenceId,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        let batch = match self.ledger.begin_service(evidence) {
            Ok(batch) => batch,
            Err(_) => {
                return DriverControlPoll::after_irq(
                    ControlProgress::Failed(InitError::InvalidState),
                    EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership),
                );
            }
        };
        let facts = batch.facts();
        if facts.contains(VirtioBlockEvidenceFacts::QUEUE) || facts.unknown_bits() != 0 {
            let _ = self.ledger.finish_service(batch, facts);
            return DriverControlPoll::after_irq(
                ControlProgress::Failed(InitError::Hardware(
                    "VirtIO queue or unknown IRQ arrived before publication",
                )),
                EvidenceServiceResult::Recover(rdif_block::ControllerFault::Protocol),
            );
        }
        let evidence_result = self
            .ledger
            .finish_service(batch, VirtioBlockEvidenceFacts::NONE);
        let poll = self
            .inner
            .poll_init(InitInput::at(now_ns), self.irq_state.is_enabled());
        let progress = self.finish_init_poll(poll, publication);
        if matches!(progress, ControlProgress::PublicationReady(_))
            && !matches!(evidence_result, EvidenceServiceResult::Drained)
        {
            return DriverControlPoll::after_irq(
                ControlProgress::Failed(InitError::Hardware(
                    "VirtIO initialization retained IRQ evidence at publication",
                )),
                EvidenceServiceResult::Recover(rdif_block::ControllerFault::Protocol),
            );
        }
        DriverControlPoll::after_irq(progress, evidence_result)
    }

    fn finish_init_poll(
        &mut self,
        poll: InitPoll<()>,
        publication: &ControllerPublicationFactory<'_>,
    ) -> ControlProgress {
        match poll {
            InitPoll::Ready(()) => self.publish_ready(publication),
            InitPoll::Pending(schedule) => match control_schedule(schedule) {
                Ok(schedule) => ControlProgress::Pending(schedule),
                Err(error) => ControlProgress::Failed(error),
            },
            InitPoll::Failed(error) => ControlProgress::Failed(error),
        }
    }

    fn publish_ready(&mut self, publication: &ControllerPublicationFactory<'_>) -> ControlProgress {
        if self.published || self.notify_port.is_none() {
            return ControlProgress::Failed(InitError::InvalidState);
        }
        let notify = match self.notify_port.take() {
            Some(notify) => notify,
            None => return ControlProgress::Failed(InitError::InvalidState),
        };
        let queue = match VirtioOwnedQueue::take_ready(
            &mut self.inner,
            notify,
            self.device_key,
            self.identity.get(),
            ControllerEpoch::INITIAL,
            self.irq_state.is_enabled(),
        ) {
            Ok(queue) => queue,
            Err((error, notify)) => {
                self.notify_port = Some(notify);
                return ControlProgress::Failed(map_init_queue_error(error));
            }
        };
        let info = queue.info();
        self.io = Some(VirtioV13IoDomain::new(self.domain, self.device_key, queue));
        let device = DriverLogicalDeviceDesc::new(
            self.device_key,
            self.name,
            info.device,
            rdif_block::HardwareQueueLimits::from(info.limits),
        );
        match publication.publish_combined(vec![device], vec![]) {
            Ok(ready) => {
                self.published = true;
                ControlProgress::PublicationReady(ready)
            }
            Err(failure) => {
                self.retained_failure = Some(VirtioRetainedFailure::Publication(failure));
                ControlProgress::Failed(
                    self.retained_failure
                        .as_ref()
                        .map_or(InitError::InvalidState, VirtioRetainedFailure::init_error),
                )
            }
        }
    }

    fn service_combined_evidence(
        &mut self,
        evidence: IrqEvidenceId,
        sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError> {
        if !matches!(self.lifecycle, VirtioV13Lifecycle::Running) {
            return Ok(EvidenceServiceResult::Recover(
                rdif_block::ControllerFault::Ownership,
            ));
        }
        let ledger = Arc::clone(&self.ledger);
        let batch = ledger.begin_service(evidence).map_err(|_| BlkError::Io)?;
        let facts = batch.facts();
        if facts.unknown_bits() != 0 {
            let _ = ledger.finish_service(batch, facts);
            return Ok(EvidenceServiceResult::Recover(
                rdif_block::ControllerFault::Protocol,
            ));
        }

        let queue_consumed = if facts.contains(VirtioBlockEvidenceFacts::QUEUE) {
            let Some(io) = self.io.as_mut() else {
                let _ = ledger.finish_service(batch, facts);
                return Ok(EvidenceServiceResult::Recover(
                    rdif_block::ControllerFault::Ownership,
                ));
            };
            if io.service_queue_fact(sink).is_err() {
                let _ = ledger.finish_service(batch, facts);
                return Ok(EvidenceServiceResult::Recover(
                    rdif_block::ControllerFault::Protocol,
                ));
            }
            true
        } else {
            false
        };
        let config_consumed = if facts.contains(VirtioBlockEvidenceFacts::CONFIG) {
            match self.ready_configuration_is_stable() {
                Ok(stable) => stable,
                Err(_) => {
                    let _ = ledger.finish_service(batch, facts);
                    return Ok(EvidenceServiceResult::Recover(
                        rdif_block::ControllerFault::Protocol,
                    ));
                }
            }
        } else {
            false
        };
        let retained = facts
            .retained_by_io(queue_consumed)
            .retained_by_control(config_consumed);
        Ok(ledger.finish_service(batch, retained))
    }

    fn ready_configuration_is_stable(&mut self) -> Result<bool, BlkError> {
        let generation = self.inner.transport.read_config_generation();
        let low = self
            .inner
            .transport
            .read_config_space::<u32>(VIRTIO_BLK_CONFIG_CAPACITY_LOW)
            .map_err(|_| BlkError::Io)?;
        let high = self
            .inner
            .transport
            .read_config_space::<u32>(VIRTIO_BLK_CONFIG_CAPACITY_HIGH)
            .map_err(|_| BlkError::Io)?;
        if self.inner.transport.read_config_generation() != generation {
            return Ok(false);
        }
        let capacity = u64::from(low) | (u64::from(high) << 32);
        if capacity != self.inner.capacity {
            return Err(BlkError::Io);
        }
        Ok(true)
    }

    fn service_lifecycle(&mut self, now_ns: u64) -> DriverControlPoll {
        let progress = match self.lifecycle {
            VirtioV13Lifecycle::Resetting { .. } => {
                let poll = self.poll_dma_quiesce_impl(InitInput::at(now_ns));
                finish_quiesce_poll(poll)
            }
            VirtioV13Lifecycle::Reinitializing { .. } => {
                let poll = self.poll_reinitialize_impl(InitInput::at(now_ns));
                self.finish_reinitialize_poll(poll)
            }
            _ => ControlProgress::Failed(InitError::InvalidState),
        };
        DriverControlPoll::without_evidence(progress)
    }

    fn begin_control_quiesce(&mut self, now_ns: u64, epoch: ControllerEpoch) -> DriverControlPoll {
        if !self.published {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(
                InitError::InvalidState,
            ));
        }
        if let Err(error) = self.begin_dma_quiesce_impl(epoch) {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(error));
        }
        self.service_lifecycle(now_ns)
    }

    fn begin_control_reinitialize(
        &mut self,
        now_ns: u64,
        quiesced: rdif_block::DmaQuiesced,
    ) -> DriverControlPoll {
        if !self.published {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(
                InitError::InvalidState,
            ));
        }
        if let Err(error) = self.begin_reinitialize_impl(quiesced) {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(error));
        }
        self.service_lifecycle(now_ns)
    }

    fn begin_dma_quiesce_impl(&mut self, epoch: ControllerEpoch) -> Result<(), InitError> {
        if !matches!(
            self.lifecycle,
            VirtioV13Lifecycle::Running | VirtioV13Lifecycle::GuestOwned
        ) || self.io.as_ref().is_none_or(|io| epoch <= io.active_epoch())
        {
            return Err(InitError::InvalidState);
        }
        self.inner.set_interrupts(false);
        if let Some(io) = self.io.as_mut() {
            io.set_interrupts(false);
        }
        self.irq_state.disable();
        self.inner
            .transport
            .set_status(virtio_drivers::transport::DeviceStatus::empty());
        self.lifecycle = VirtioV13Lifecycle::Resetting {
            epoch,
            deadline_ns: None,
        };
        Ok(())
    }

    fn poll_dma_quiesce_impl(&mut self, input: InitInput) -> InitPoll<rdif_block::DmaQuiesced> {
        let VirtioV13Lifecycle::Resetting {
            epoch,
            mut deadline_ns,
        } = self.lifecycle
        else {
            return InitPoll::Failed(InitError::InvalidState);
        };
        if self.inner.transport.get_status().is_empty() {
            self.lifecycle = VirtioV13Lifecycle::Quiesced { epoch };
            return InitPoll::Ready(unsafe {
                // SAFETY: the fixed runtime owner closes dispatch and drains
                // the IRQ action before this transition. Status zero is the
                // VirtIO device-reset acknowledgement that no published
                // virtqueue or request buffer remains DMA-reachable.
                rdif_block::DmaQuiesced::new(epoch, self.identity.get())
            });
        }
        let deadline =
            *deadline_ns.get_or_insert_with(|| input.now_ns.saturating_add(1_000_000_000));
        if input.now_ns >= deadline {
            self.lifecycle = VirtioV13Lifecycle::Failed;
            return InitPoll::Failed(InitError::TimedOut);
        }
        self.lifecycle = VirtioV13Lifecycle::Resetting { epoch, deadline_ns };
        InitPoll::Pending(InitSchedule::wait_until(
            input.now_ns.saturating_add(50_000).min(deadline),
        ))
    }

    fn begin_reinitialize_impl(&mut self, proof: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        let VirtioV13Lifecycle::Quiesced { epoch } = self.lifecycle else {
            return Err(InitError::InvalidState);
        };
        if proof.epoch() != epoch || proof.controller_cookie() != self.identity.get() {
            return Err(InitError::InvalidState);
        }
        let io = self.io.as_mut().ok_or(InitError::InvalidState)?;
        let (notify, descriptor_storage) = io
            .begin_rebuild(epoch)
            .map_err(|_| InitError::InvalidState)?;
        self.notify_port = Some(notify);
        self.inner.descriptor_storage = Some(descriptor_storage);
        if let Err(error) = self.inner.prepare_reinitialize() {
            self.lifecycle = VirtioV13Lifecycle::Failed;
            return Err(error);
        }
        self.lifecycle = VirtioV13Lifecycle::Reinitializing { epoch };
        Ok(())
    }

    fn poll_reinitialize_impl(&mut self, input: InitInput) -> InitPoll<ControllerReady> {
        let VirtioV13Lifecycle::Reinitializing { epoch } = self.lifecycle else {
            return InitPoll::Failed(InitError::InvalidState);
        };
        match self.inner.poll_init(input, false) {
            InitPoll::Ready(()) => {
                let Some(notify) = self.notify_port.take() else {
                    self.lifecycle = VirtioV13Lifecycle::Failed;
                    return InitPoll::Failed(InitError::InvalidState);
                };
                let queue = match VirtioOwnedQueue::take_ready(
                    &mut self.inner,
                    notify,
                    self.device_key,
                    self.identity.get(),
                    epoch,
                    false,
                ) {
                    Ok(queue) => queue,
                    Err((error, notify)) => {
                        self.notify_port = Some(notify);
                        self.lifecycle = VirtioV13Lifecycle::Failed;
                        return InitPoll::Failed(map_init_queue_error(error));
                    }
                };
                let Some(io) = self.io.as_mut() else {
                    self.retained_failure = Some(VirtioRetainedFailure::RebuiltQueue {
                        _queue: Box::new(queue),
                    });
                    self.lifecycle = VirtioV13Lifecycle::Failed;
                    return InitPoll::Failed(InitError::InvalidState);
                };
                if let Err(queue) = io.install_rebuilt_queue(epoch, queue) {
                    self.retained_failure =
                        Some(VirtioRetainedFailure::RebuiltQueue { _queue: queue });
                    self.lifecycle = VirtioV13Lifecycle::Failed;
                    return InitPoll::Failed(InitError::InvalidState);
                }
                InitPoll::Ready(unsafe {
                    // SAFETY: the normal initializer negotiated the retained
                    // geometry, rebuilt fresh queue DMA, installed the queue
                    // in its same combined owner, and set DRIVER_OK. IRQ and
                    // remote admission remain closed until the domain consumes
                    // the matching reinitialization permit.
                    ControllerReady::new(epoch, self.identity.get())
                })
            }
            InitPoll::Pending(schedule) => InitPoll::Pending(schedule),
            InitPoll::Failed(error) => {
                self.lifecycle = VirtioV13Lifecycle::Failed;
                InitPoll::Failed(error)
            }
        }
    }

    fn finish_reinitialize_poll(&mut self, poll: InitPoll<ControllerReady>) -> ControlProgress {
        match poll {
            InitPoll::Ready(ready) => {
                match ControllerReinitialized::new(ready, vec![self.domain]) {
                    Ok(reinitialized) => ControlProgress::Reinitialized(reinitialized),
                    Err(_) => ControlProgress::Failed(InitError::Hardware(
                        "VirtIO reinitialization domain proof set is invalid",
                    )),
                }
            }
            InitPoll::Pending(schedule) => match control_schedule(schedule) {
                Ok(schedule) => ControlProgress::Pending(schedule),
                Err(error) => ControlProgress::Failed(error),
            },
            InitPoll::Failed(error) => ControlProgress::Failed(error),
        }
    }
}

impl<T: VirtIoTransport> DriverGeneric for VirtioV13Control<T> {
    fn name(&self) -> &str {
        self.name
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl<T: VirtIoTransport> ControllerControl for VirtioV13Control<T> {
    fn controller_identity(&self) -> NonZeroUsize {
        self.identity
    }

    fn service_control(
        &mut self,
        trigger: DriverControlTrigger,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        if let Some(failure) = &self.retained_failure {
            let progress = ControlProgress::Failed(failure.init_error());
            return if matches!(trigger, DriverControlTrigger::Irq { .. }) {
                DriverControlPoll::after_irq(
                    progress,
                    EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership),
                )
            } else {
                DriverControlPoll::without_evidence(progress)
            };
        }
        match trigger {
            DriverControlTrigger::Start { now_ns } => {
                if self.published {
                    DriverControlPoll::without_evidence(ControlProgress::Failed(
                        InitError::InvalidState,
                    ))
                } else {
                    self.service_init(now_ns, publication)
                }
            }
            DriverControlTrigger::InternalProgress { now_ns }
            | DriverControlTrigger::ProtocolDeadline { now_ns } => {
                if self.published {
                    self.service_lifecycle(now_ns)
                } else {
                    self.service_init(now_ns, publication)
                }
            }
            DriverControlTrigger::Irq { now_ns, evidence } => {
                if self.published {
                    DriverControlPoll::after_irq(
                        ControlProgress::Failed(InitError::InvalidState),
                        EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership),
                    )
                } else {
                    self.service_init_irq(now_ns, evidence, publication)
                }
            }
            DriverControlTrigger::BeginQuiesce {
                now_ns,
                intent: _,
                epoch,
            } => self.begin_control_quiesce(now_ns, epoch),
            DriverControlTrigger::BeginReinitialize { now_ns, quiesced } => {
                self.begin_control_reinitialize(now_ns, quiesced)
            }
        }
    }

    fn service_ready_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<EvidenceServiceResult, BlkError> {
        if !self.published || self.retained_failure.is_some() {
            return Ok(EvidenceServiceResult::Recover(
                rdif_block::ControllerFault::Ownership,
            ));
        }
        let _ = evidence;
        Ok(EvidenceServiceResult::Recover(
            rdif_block::ControllerFault::Ownership,
        ))
    }

    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        self.ledger
            .commit_drained_evidence(evidence)
            .map_err(|_| BlkError::Io)
    }

    fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        self.ledger.retire_after_quiesce(permit, self.identity)
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        LifecycleEndpoint::Interrupt(self)
    }

    fn enable_irq(&mut self) -> Result<(), BlkError> {
        self.irq_state.enable();
        self.inner.set_interrupts(true);
        if let Some(io) = self.io.as_mut() {
            io.set_interrupts(true);
        }
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<(), BlkError> {
        self.inner.set_interrupts(false);
        if let Some(io) = self.io.as_mut() {
            io.set_interrupts(false);
        }
        self.irq_state.disable();
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_state.is_enabled()
    }
}

impl<T: VirtIoTransport> InterruptLifecycle for VirtioV13Control<T> {
    fn controller_cookie(&self) -> usize {
        self.identity.get()
    }

    fn begin_dma_quiesce(
        &mut self,
        epoch: ControllerEpoch,
        _cause: RecoveryCause,
    ) -> Result<(), InitError> {
        self.begin_dma_quiesce_impl(epoch)
    }

    fn poll_dma_quiesce(&mut self, input: InitInput) -> InitPoll<rdif_block::DmaQuiesced> {
        self.poll_dma_quiesce_impl(input)
    }

    fn enter_guest_owned(&mut self, proof: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        let VirtioV13Lifecycle::Quiesced { epoch } = self.lifecycle else {
            return Err(InitError::InvalidState);
        };
        if proof.epoch() != epoch
            || proof.controller_cookie() != self.identity.get()
            || self
                .io
                .as_ref()
                .is_none_or(|io| !io.reclaimed_epoch_matches(epoch))
        {
            return Err(InitError::InvalidState);
        }
        self.lifecycle = VirtioV13Lifecycle::GuestOwned;
        Ok(())
    }

    fn begin_reinitialize(&mut self, proof: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        self.begin_reinitialize_impl(proof)
    }

    fn poll_reinitialize(&mut self, input: InitInput) -> InitPoll<rdif_block::ControllerReady> {
        self.poll_reinitialize_impl(input)
    }
}

impl<T: VirtIoTransport> InterruptIoDomain for VirtioV13Control<T> {
    fn domain_id(&self) -> OwnershipDomainId {
        self.io
            .as_ref()
            .map_or(self.domain, VirtioV13IoDomain::domain_id)
    }

    fn queue_count(&self) -> usize {
        self.io.as_ref().map_or(1, VirtioV13IoDomain::queue_count)
    }

    fn submit_owned(
        &mut self,
        queue_id: usize,
        logical_device: DriverDeviceKey,
        id: rdif_block::RequestId,
        request: rdif_block::OwnedRequest,
    ) -> Result<rdif_block::AcceptedRequest, UnacceptedRequest> {
        let Some(io) = self.io.as_mut() else {
            return Err(UnacceptedRequest::new(id, BlkError::Offline, request));
        };
        io.submit_owned(queue_id, logical_device, id, request)
    }

    fn service_evidence(
        &mut self,
        evidence: IrqEvidenceId,
        sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError> {
        self.service_combined_evidence(evidence, sink)
    }

    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        self.ledger
            .commit_drained_evidence(evidence)
            .map_err(|_| BlkError::Io)
    }

    fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        self.ledger.retire_after_quiesce(permit, self.identity)
    }

    fn reclaim_after_quiesce(
        &mut self,
        proof: &rdif_block::DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        self.io
            .as_mut()
            .ok_or(BlkError::Offline)?
            .reclaim_after_quiesce(proof, sink)
    }

    fn resume_after_reinitialize(&mut self, epoch: ControllerEpoch) -> Result<(), BlkError> {
        if !matches!(
            self.lifecycle,
            VirtioV13Lifecycle::Reinitializing { epoch: active } if active == epoch
        ) {
            return Err(BlkError::InvalidDmaProof);
        }
        self.io
            .as_mut()
            .ok_or(BlkError::Offline)?
            .resume_after_reinitialize(epoch)?;
        self.lifecycle = VirtioV13Lifecycle::Running;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        self.io.as_mut().ok_or(BlkError::Offline)?.shutdown()
    }
}

impl<T: VirtIoTransport> SharedControllerIoDomain for VirtioV13Control<T> {
    fn io_domain_mut(&mut self) -> &mut dyn InterruptIoDomain {
        self
    }
}

fn virtio_capabilities(
    identity: NonZeroUsize,
    domain: OwnershipDomainId,
    source: IrqSourceId,
) -> Result<ControllerCapabilities, ActivationError> {
    let mut sources = IdList::none();
    sources.insert(source.get());
    let domain_capability = OwnershipDomainCapability::new(
        domain,
        LogicalDeviceSelector::AllPublished,
        QueueExecution::Tagged,
        NonZeroU16::MIN,
        NonZeroU16::MIN,
        HardwareQueueDepth::fixed(NonZeroU16::MIN),
        sources,
    )?;
    let control = ControlDomainCapability::shared_with_io(domain, sources)?;
    ControllerCapabilities::new_discovering(
        identity,
        control,
        NonZeroU16::MIN,
        LogicalDeviceConstraints::discover_during_init(
            dma_api::DmaDomainId::legacy_global(),
            u64::MAX,
        ),
        OwnershipDomainIds::from_bits(1_u64 << domain.get()),
        vec![domain_capability],
    )
}

fn control_schedule(schedule: InitSchedule) -> Result<ControlSchedule, InitError> {
    ControlSchedule::new(
        schedule.run_again(),
        schedule.irq_sources(),
        schedule.wake_at_ns(),
    )
}

fn finish_quiesce_poll(poll: InitPoll<rdif_block::DmaQuiesced>) -> ControlProgress {
    match poll {
        InitPoll::Ready(proof) => ControlProgress::DmaQuiesced(proof),
        InitPoll::Pending(schedule) => match control_schedule(schedule) {
            Ok(schedule) => ControlProgress::Pending(schedule),
            Err(error) => ControlProgress::Failed(error),
        },
        InitPoll::Failed(error) => ControlProgress::Failed(error),
    }
}

fn map_init_queue_error(error: BlkError) -> InitError {
    match error {
        BlkError::NoMemory => InitError::Hardware("VirtIO block queue allocation failed"),
        _ => InitError::Hardware("VirtIO block queue publication failed"),
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec};
    use core::{
        cell::Cell,
        mem::{MaybeUninit, size_of},
        num::{NonZeroU16, NonZeroU64},
        sync::atomic::{AtomicU8, AtomicUsize},
    };

    use rdif_block::{
        ActivationError, ActivationPlan, CompletionSink, ControlProgress, ControlTrigger,
        ControllerActivator, ControllerEpoch, DomainActivationPlan, DomainOwnerBinding,
        QuiesceIntent,
    };
    use virtio_drivers::{
        Error as VirtIoError, PhysAddr,
        transport::{DeviceStatus, DeviceType, InterruptStatus, Transport},
    };

    use super::{
        VIRTIO_BLK_CONFIG_CAPACITY_HIGH, VIRTIO_BLK_CONFIG_CAPACITY_LOW, VirtioBlockActivator,
    };
    use crate::virtio::block::{
        irq::test_interrupt_port, notify::VirtioQueueNotifyPort, tests::RecordingTransport,
    };

    #[test]
    fn activation_advertises_only_the_single_stable_descriptor_credit() {
        let activator = VirtioBlockActivator::discovered(
            "virtio-blk-test",
            RecordingTransport::new(Arc::new(AtomicUsize::new(0))),
            test_interrupt_port(Arc::new(AtomicU8::new(0))),
            VirtioQueueNotifyPort::for_test(Arc::new(AtomicUsize::new(0))),
        )
        .expect("command-free discovery must describe the controller");
        let capabilities = activator.capabilities();
        let domain = capabilities.domains()[0].id();
        let queue_depth = capabilities.domains()[0].queue_depth();

        assert_eq!(capabilities.domains()[0].min_queues(), NonZeroU16::MIN);
        assert_eq!(capabilities.domains()[0].max_queues(), NonZeroU16::MIN);
        assert_eq!(queue_depth.min(), NonZeroU16::MIN);
        assert_eq!(queue_depth.max(), NonZeroU16::MIN);

        let invalid = ActivationPlan::new(
            capabilities,
            alloc::vec![DomainActivationPlan::new(
                domain,
                NonZeroU16::MIN,
                NonZeroU16::new(2).unwrap(),
                capabilities.domains()[0].irq_sources(),
            )],
        );
        assert_eq!(
            invalid,
            Err(ActivationError::QueueDepthOutOfRange { domain })
        );
    }

    #[test]
    fn control_triggers_quiesce_rebuild_and_resume_the_same_queue_owner() {
        let activator = VirtioBlockActivator::discovered(
            "virtio-blk-test",
            ReadyTransport::new(8_192),
            test_interrupt_port(Arc::new(AtomicU8::new(0))),
            VirtioQueueNotifyPort::for_test(Arc::new(AtomicUsize::new(0))),
        )
        .unwrap();
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
        let registration = prepared.control_mut().owned_irq_sources_mut()[0]
            .take_for_registration()
            .unwrap();
        prepared.control_mut().owned_irq_sources_mut()[0]
            .finish_registration()
            .unwrap();
        prepared.enable_irq().unwrap();
        let ready = drive_initial_publication(&mut prepared);
        let staged = prepared.stage(ready).unwrap();
        let (mut coordinator, mut domains) = staged.into_installations();
        let owner = DomainOwnerBinding::new(0, NonZeroU64::MIN);
        let mut split_domain = if let Some(domain) = domains.pop() {
            let (installed, proof) = domain.finish_binding(owner).unwrap();
            coordinator.accept_bound_domain(proof).unwrap();
            Some(installed)
        } else {
            coordinator.bind_combined_control_domain(owner).unwrap();
            None
        };
        assert!(domains.is_empty());
        let mut published = coordinator.publish().unwrap();
        drop(registration);

        assert!(matches!(
            published
                .control_mut()
                .service_control(ControlTrigger::BeginQuiesce {
                    now_ns: 99,
                    intent: QuiesceIntent::Recovery(rdif_block::ControllerFault::Protocol),
                    epoch: ControllerEpoch::INITIAL,
                })
                .unwrap()
                .into_parts()
                .0,
            ControlProgress::Failed(rdif_block::InitError::InvalidState)
        ));
        let quiesced = match published
            .control_mut()
            .service_control(ControlTrigger::BeginQuiesce {
                now_ns: 100,
                intent: QuiesceIntent::Recovery(rdif_block::ControllerFault::Protocol),
                epoch: ControllerEpoch::new(2),
            })
            .unwrap()
            .into_parts()
            .0
        {
            ControlProgress::DmaQuiesced(proof) => proof,
            progress => panic!("VirtIO reset did not publish a DMA proof: {progress:?}"),
        };
        let mut completions = RejectCompletions;
        if let Some(domain) = split_domain.as_mut() {
            domain
                .io_mut()
                .reclaim_after_quiesce(&quiesced, &mut completions)
                .unwrap();
        } else {
            published
                .shared_io_domain_mut()
                .unwrap()
                .reclaim_after_quiesce(&quiesced, &mut completions)
                .unwrap();
        }

        let mut progress = published
            .control_mut()
            .service_control(ControlTrigger::BeginReinitialize {
                now_ns: 101,
                quiesced,
            })
            .unwrap()
            .into_parts()
            .0;
        let mut now_ns = 101_u64;
        for _ in 0..16 {
            let ControlProgress::Pending(schedule) = progress else {
                break;
            };
            now_ns = schedule
                .wake_at_ns()
                .unwrap_or_else(|| now_ns.saturating_add(1));
            let trigger = if schedule.internal_progress_ready() {
                ControlTrigger::InternalProgress { now_ns }
            } else {
                ControlTrigger::ProtocolDeadline { now_ns }
            };
            progress = published
                .control_mut()
                .service_control(trigger)
                .unwrap()
                .into_parts()
                .0;
        }
        let ControlProgress::Reinitialized(reinitialized) = progress else {
            panic!("VirtIO full rebuild did not publish a ready proof: {progress:?}")
        };
        let reinitialized = published
            .control()
            .bind_reinitialized(reinitialized)
            .unwrap();
        let (mut epoch_commit, mut permits) = reinitialized.into_resume_parts();
        let permit = permits.pop().unwrap();
        assert!(permits.is_empty());
        let resumed = if let Some(domain) = split_domain.as_mut() {
            domain.resume_after_reinitialize(permit).unwrap()
        } else {
            published
                .resume_shared_io_after_reinitialize(permit)
                .unwrap()
        };
        epoch_commit.accept_resumed(resumed).unwrap();
        let epoch_commit = epoch_commit.finish().unwrap();
        assert_eq!(
            published.commit_reinitialized_epoch(epoch_commit).unwrap(),
            ControllerEpoch::new(2)
        );
        published.control_mut().enable_irq().unwrap();
        assert!(published.control().is_irq_enabled());
    }

    fn drive_initial_publication(
        prepared: &mut rdif_block::PreparedControllerParts,
    ) -> rdif_block::ControllerPublicationReady {
        let mut progress = prepared
            .service_control(ControlTrigger::Start { now_ns: 0 })
            .unwrap()
            .into_parts()
            .0;
        let mut now_ns = 0_u64;
        for _ in 0..16 {
            match progress {
                ControlProgress::PublicationReady(ready) => return ready,
                ControlProgress::Pending(schedule) => {
                    now_ns = schedule
                        .wake_at_ns()
                        .unwrap_or_else(|| now_ns.saturating_add(1));
                    let trigger = if schedule.internal_progress_ready() {
                        ControlTrigger::InternalProgress { now_ns }
                    } else {
                        ControlTrigger::ProtocolDeadline { now_ns }
                    };
                    progress = prepared.service_control(trigger).unwrap().into_parts().0;
                }
                progress => panic!("VirtIO initialization failed: {progress:?}"),
            }
        }
        panic!("VirtIO initialization exceeded its bounded transition budget")
    }

    struct RejectCompletions;

    impl CompletionSink for RejectCompletions {
        fn complete(&mut self, _completion: rdif_block::CompletedRequest) {
            panic!("idle lifecycle test must not manufacture a completion")
        }
    }

    struct ReadyTransport {
        status: Cell<DeviceStatus>,
        capacity: u64,
    }

    impl ReadyTransport {
        const fn new(capacity: u64) -> Self {
            Self {
                status: Cell::new(DeviceStatus::empty()),
                capacity,
            }
        }
    }

    impl Transport for ReadyTransport {
        fn device_type(&self) -> DeviceType {
            DeviceType::Block
        }

        fn read_device_features(&mut self) -> u64 {
            1 << 32
        }

        fn write_driver_features(&mut self, _driver_features: u64) {}

        fn max_queue_size(&mut self, _queue: u16) -> u32 {
            16
        }

        fn notify(&mut self, _queue: u16) {}

        fn get_status(&self) -> DeviceStatus {
            self.status.get()
        }

        fn set_status(&mut self, status: DeviceStatus) {
            self.status.set(status);
        }

        fn set_guest_page_size(&mut self, _guest_page_size: u32) {}

        fn requires_legacy_layout(&self) -> bool {
            false
        }

        fn queue_set(
            &mut self,
            _queue: u16,
            _size: u32,
            _descriptors: PhysAddr,
            _driver_area: PhysAddr,
            _device_area: PhysAddr,
        ) {
        }

        fn queue_unset(&mut self, _queue: u16) {}

        fn queue_used(&mut self, _queue: u16) -> bool {
            false
        }

        fn ack_interrupt(&mut self) -> InterruptStatus {
            InterruptStatus::empty()
        }

        fn read_config_generation(&self) -> u32 {
            0
        }

        fn read_config_space<T>(&self, offset: usize) -> virtio_drivers::Result<T> {
            if size_of::<T>() != size_of::<u32>() {
                return Err(VirtIoError::ConfigSpaceMissing);
            }
            let word = match offset {
                VIRTIO_BLK_CONFIG_CAPACITY_LOW => self.capacity as u32,
                VIRTIO_BLK_CONFIG_CAPACITY_HIGH => (self.capacity >> 32) as u32,
                _ => return Err(VirtIoError::ConfigSpaceMissing),
            };
            let mut value = MaybeUninit::<T>::uninit();
            unsafe {
                // SAFETY: Transport requires T: FromBytes. The size check
                // proves the initialized u32 bytes cover the entire value.
                core::ptr::copy_nonoverlapping(
                    core::ptr::from_ref(&word).cast::<u8>(),
                    value.as_mut_ptr().cast::<u8>(),
                    size_of::<u32>(),
                );
                Ok(value.assume_init())
            }
        }

        fn write_config_space<T>(
            &mut self,
            _offset: usize,
            _value: T,
        ) -> virtio_drivers::Result<()> {
            Err(VirtIoError::Unsupported)
        }
    }
}
