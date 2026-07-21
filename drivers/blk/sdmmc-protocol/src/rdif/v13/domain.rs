//! One physical SD/MMC owner across initialization, I/O, and recovery.

use alloc::{boxed::Box, sync::Arc, vec};
use core::{any::Any, mem, num::NonZeroUsize};

use rdif_block::{
    BlkError, CompletionSink, ControlProgress, ControlSchedule, ControllerControl, ControllerFault,
    ControllerPublicationFactory, ControllerReinitialized, DriverControlPoll, DriverControlTrigger,
    DriverDeviceKey, DriverEvidenceRetirement, DriverGeneric, DriverLogicalDeviceDesc,
    EvidenceServiceResult, HardwareQueueLimits, InitError, InitInput as RuntimeInitInput,
    InitPoll as RuntimeInitPoll, InterruptIoDomain, InterruptLifecycle, IrqEvidenceId,
    LifecycleEndpoint, OwnedRequest, OwnershipDomainId, QuiesceIntent, RecoveryCause,
    RecoveryEvidenceRetireFailure, RecoveryEvidenceRetirePermit, RecoveryEvidenceRetired,
    RequestId, SharedControllerIoDomain, UnacceptedRequest,
};

use super::{
    SdmmcEvidenceDisposition, SdmmcEvidenceEpoch, SdmmcEvidenceLedger, SdmmcIrqFacts,
    activation::SdmmcActivationPrelude, queue::SdmmcRequestQueue,
};
use crate::{
    rdif::{BlockConfig, config::map_dev_err_to_blk_err, host::BlockHost, queue_limits},
    sdio::{InitInput, InitIrqWait, InitPoll, OwnedSdioInit, OwnedSdioInitHost, SdioHost},
};

pub(super) const SDMMC_DRIVER_DEVICE_KEY: DriverDeviceKey =
    DriverDeviceKey::new(core::num::NonZeroU64::MIN);

/// Allocation retained for the entire controller lifetime so the numeric RDIF
/// identity cannot be reused while this owner still exists.
pub(super) struct ControllerIdentity {
    _occupied: u8,
}

impl ControllerIdentity {
    pub(super) fn allocate() -> (Box<Self>, NonZeroUsize) {
        let owner = Box::new(Self { _occupied: 1 });
        let address = core::ptr::from_ref(owner.as_ref()).expose_provenance();
        let identity = NonZeroUsize::new(address)
            .unwrap_or_else(|| unreachable!("a live Box has a nonzero address"));
        (owner, identity)
    }
}

pub(super) struct CombinedSdmmcDomain<H>
where
    H: BlockHost + OwnedSdioInitHost,
{
    name: &'static str,
    identity: NonZeroUsize,
    _identity_owner: Box<ControllerIdentity>,
    domain: OwnershipDomainId,
    ledger: Arc<SdmmcEvidenceLedger>,
    evidence_epoch: Arc<SdmmcEvidenceEpoch>,
    prelude: PreludeSession,
    state: DomainState<H>,
}

pub(super) struct CombinedSdmmcDomainParts<H>
where
    H: BlockHost + OwnedSdioInitHost,
{
    pub(super) identity_owner: Box<ControllerIdentity>,
    pub(super) identity: NonZeroUsize,
    pub(super) domain: OwnershipDomainId,
    pub(super) init: Box<OwnedSdioInit<H>>,
    pub(super) config: BlockConfig,
    pub(super) ledger: Arc<SdmmcEvidenceLedger>,
    pub(super) evidence_epoch: Arc<SdmmcEvidenceEpoch>,
    pub(super) prelude: Box<dyn SdmmcActivationPrelude>,
}

struct PreludeSession {
    owner: Box<dyn SdmmcActivationPrelude>,
    state: PreludeState,
    irq_requested: bool,
}

#[derive(Clone, Copy)]
enum PreludeState {
    Prepare,
    Settling { wake_at_ns: u64 },
    Ready,
    Failed,
}

enum PreludeProgress {
    Pending(ControlSchedule),
    Ready,
}

#[derive(Clone, Copy)]
enum LifecycleProgress {
    None,
    Quiescing,
    Reinitializing,
}

pub(super) enum DomainState<H>
where
    H: BlockHost + OwnedSdioInitHost,
{
    Initial {
        init: Box<OwnedSdioInit<H>>,
        config: BlockConfig,
    },
    Ready(ReadyDomain<H>),
    Failed(Option<ReadyDomain<H>>),
    Transitioning,
}

pub(super) struct ReadyDomain<H: BlockHost> {
    pub(super) card: crate::sdio::SdioSdmmc<H>,
    pub(super) config: BlockConfig,
    pub(super) queue: SdmmcRequestQueue<H>,
    pub(super) recovery: RecoveryState<H::RecoveryState>,
    pub(super) epoch: rdif_block::ControllerEpoch,
    pub(super) fault: Option<ControllerFault>,
}

pub(super) enum RecoveryState<R> {
    Idle,
    GuestOwned,
    Quiescing {
        epoch: rdif_block::ControllerEpoch,
        host: R,
    },
    Quiesced {
        epoch: rdif_block::ControllerEpoch,
        host: R,
    },
    Reinitializing {
        epoch: rdif_block::ControllerEpoch,
        host: R,
    },
}

impl<H> CombinedSdmmcDomain<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    pub(super) fn new(parts: CombinedSdmmcDomainParts<H>) -> Self {
        let CombinedSdmmcDomainParts {
            identity_owner,
            identity,
            domain,
            init,
            config,
            ledger,
            evidence_epoch,
            prelude,
        } = parts;
        Self {
            name: config.name,
            identity,
            _identity_owner: identity_owner,
            domain,
            ledger,
            evidence_epoch,
            prelude: PreludeSession {
                owner: prelude,
                state: PreludeState::Prepare,
                irq_requested: false,
            },
            state: DomainState::Initial { init, config },
        }
    }

    fn service_without_irq(
        &mut self,
        now_ns: u64,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        match self.lifecycle_progress() {
            LifecycleProgress::Quiescing => return self.poll_quiesce(now_ns),
            LifecycleProgress::Reinitializing => return self.poll_reinitialize(now_ns),
            LifecycleProgress::None => {}
        }
        if !self.prelude.irq_requested {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(
                InitError::MissingInterrupt,
            ));
        }
        match self.prelude.advance(now_ns) {
            Ok(PreludeProgress::Pending(schedule)) => {
                return DriverControlPoll::without_evidence(ControlProgress::Pending(schedule));
            }
            Ok(PreludeProgress::Ready) => {}
            Err(error) => {
                return DriverControlPoll::without_evidence(ControlProgress::Failed(error));
            }
        }
        let poll = match &mut self.state {
            DomainState::Initial { init, .. } => {
                if !init.completion_irq_enabled() && init.enable_completion_irq().is_err() {
                    return DriverControlPoll::without_evidence(ControlProgress::Failed(
                        InitError::Hardware("SD/MMC controller IRQ enable failed"),
                    ));
                }
                init.poll_init(InitInput::at(now_ns))
            }
            DomainState::Ready(_) | DomainState::Failed(_) | DomainState::Transitioning => {
                return DriverControlPoll::without_evidence(ControlProgress::Failed(
                    InitError::InvalidState,
                ));
            }
        };
        DriverControlPoll::without_evidence(self.finish_init_poll(poll, publication))
    }

    fn lifecycle_progress(&self) -> LifecycleProgress {
        let ready = match &self.state {
            DomainState::Ready(ready) | DomainState::Failed(Some(ready)) => ready,
            DomainState::Initial { .. }
            | DomainState::Failed(None)
            | DomainState::Transitioning => return LifecycleProgress::None,
        };
        match ready.recovery {
            RecoveryState::Quiescing { .. } => LifecycleProgress::Quiescing,
            RecoveryState::Reinitializing { .. } => LifecycleProgress::Reinitializing,
            RecoveryState::Idle | RecoveryState::GuestOwned | RecoveryState::Quiesced { .. } => {
                LifecycleProgress::None
            }
        }
    }

    fn begin_quiesce(
        &mut self,
        now_ns: u64,
        intent: QuiesceIntent,
        epoch: rdif_block::ControllerEpoch,
    ) -> DriverControlPoll {
        let cause = match intent {
            QuiesceIntent::Shutdown => RecoveryCause::Shutdown,
            QuiesceIntent::OwnershipTransfer => RecoveryCause::Handoff,
            QuiesceIntent::Recovery(_) => RecoveryCause::QueueFault { queue_id: 0 },
        };
        if let Err(error) = InterruptLifecycle::begin_dma_quiesce(self, epoch, cause) {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(error));
        }
        self.poll_quiesce(now_ns)
    }

    fn poll_quiesce(&mut self, now_ns: u64) -> DriverControlPoll {
        let progress =
            match InterruptLifecycle::poll_dma_quiesce(self, RuntimeInitInput::at(now_ns)) {
                RuntimeInitPoll::Ready(proof) => ControlProgress::DmaQuiesced(proof),
                RuntimeInitPoll::Pending(schedule) => lifecycle_schedule(schedule),
                RuntimeInitPoll::Failed(error) => ControlProgress::Failed(error),
            };
        DriverControlPoll::without_evidence(progress)
    }

    fn begin_reinitialize(
        &mut self,
        now_ns: u64,
        quiesced: rdif_block::DmaQuiesced,
    ) -> DriverControlPoll {
        if let Err(error) = InterruptLifecycle::begin_reinitialize(self, quiesced) {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(error));
        }
        self.poll_reinitialize(now_ns)
    }

    fn poll_reinitialize(&mut self, now_ns: u64) -> DriverControlPoll {
        let progress =
            match InterruptLifecycle::poll_reinitialize(self, RuntimeInitInput::at(now_ns)) {
                RuntimeInitPoll::Ready(ready) => {
                    match ControllerReinitialized::new(ready, vec![self.domain]) {
                        Ok(reinitialized) => ControlProgress::Reinitialized(reinitialized),
                        Err(_) => ControlProgress::Failed(InitError::Hardware(
                            "SD/MMC reinitialization domain proof is invalid",
                        )),
                    }
                }
                RuntimeInitPoll::Pending(schedule) => lifecycle_schedule(schedule),
                RuntimeInitPoll::Failed(error) => ControlProgress::Failed(error),
            };
        DriverControlPoll::without_evidence(progress)
    }

    fn service_initial_irq(
        &mut self,
        now_ns: u64,
        evidence: IrqEvidenceId,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        let ledger = Arc::clone(&self.ledger);
        let batch = match ledger.begin_service(evidence) {
            Ok(batch) => batch,
            Err(_) => return initialization_recovery(ControllerFault::Ownership),
        };
        if batch.facts().has_overflow() {
            let _ = ledger.finish_service(batch, SdmmcIrqFacts::none());
            return initialization_recovery(ControllerFault::Protocol);
        }
        let snapshot = batch.facts().snapshot();
        let poll = match &mut self.state {
            DomainState::Initial { init, .. } => {
                init.poll_init(InitInput::with_controller_snapshot(now_ns, snapshot))
            }
            DomainState::Ready(_) | DomainState::Failed(_) | DomainState::Transitioning => {
                let _ = ledger.finish_service(batch, SdmmcIrqFacts::none());
                return initialization_recovery(ControllerFault::Ownership);
            }
        };
        let disposition = ledger.finish_service(batch, SdmmcIrqFacts::none());
        let evidence_result = evidence_result(disposition);
        if matches!(poll, InitPoll::Ready(_))
            && !matches!(evidence_result, EvidenceServiceResult::Drained)
        {
            return initialization_recovery(ControllerFault::Protocol);
        }
        DriverControlPoll::after_irq(self.finish_init_poll(poll, publication), evidence_result)
    }

    fn finish_init_poll(
        &mut self,
        poll: InitPoll<crate::sdio::CardInfo>,
        publication: &ControllerPublicationFactory<'_>,
    ) -> ControlProgress {
        match poll {
            InitPoll::Ready(_) => self.finish_initialization(publication),
            InitPoll::Pending(schedule) => {
                let mut sources = rdif_block::IdList::none();
                if matches!(schedule.irq, InitIrqWait::Controller) {
                    sources.insert(0);
                }
                match ControlSchedule::new(schedule.run_again, sources, schedule.wake_at_ns) {
                    Ok(schedule) => ControlProgress::Pending(schedule),
                    Err(error) => ControlProgress::Failed(error),
                }
            }
            InitPoll::Failed(error) => ControlProgress::Failed(map_init_error(error)),
        }
    }

    fn finish_initialization(
        &mut self,
        publication: &ControllerPublicationFactory<'_>,
    ) -> ControlProgress {
        let state = mem::replace(&mut self.state, DomainState::Transitioning);
        let DomainState::Initial { init, mut config } = state else {
            self.state = state;
            return ControlProgress::Failed(InitError::InvalidState);
        };
        let initialized = match init.try_into_ready() {
            Ok(initialized) => initialized,
            Err(init) => {
                self.state = DomainState::Initial { init, config };
                return ControlProgress::Failed(InitError::InvalidState);
            }
        };
        let Some(capacity_blocks) = initialized.card_info().capacity_blocks else {
            let (mut card, _) = initialized.into_parts();
            card.host_mut().prepare_block_runtime();
            self.state = DomainState::Failed(Some(ReadyDomain::new(card, config)));
            return ControlProgress::Failed(InitError::Hardware(
                "SD/MMC card did not publish a usable capacity",
            ));
        };
        config.capacity_blocks = capacity_blocks;
        let (mut card, _) = initialized.into_parts();
        card.host_mut().prepare_block_runtime();
        let ready = ReadyDomain::new(card, config);
        let logical_device = DriverLogicalDeviceDesc::new(
            SDMMC_DRIVER_DEVICE_KEY,
            self.name,
            ready.device_info(),
            ready.queue_limits(),
        );
        self.state = DomainState::Ready(ready);
        match publication.publish_combined(vec![logical_device], vec![]) {
            Ok(ready) => ControlProgress::PublicationReady(ready),
            Err(_) => {
                let ready = match mem::replace(&mut self.state, DomainState::Transitioning) {
                    DomainState::Ready(ready) => ready,
                    _ => unreachable!("publication follows a successful ready transition"),
                };
                self.state = DomainState::Failed(Some(ready));
                ControlProgress::Failed(InitError::Hardware(
                    "SD/MMC Ready publication violated the activation plan",
                ))
            }
        }
    }

    pub(super) fn ready_mut(&mut self) -> Result<&mut ReadyDomain<H>, BlkError> {
        match &mut self.state {
            DomainState::Ready(ready) => Ok(ready),
            DomainState::Initial { .. } | DomainState::Failed(_) | DomainState::Transitioning => {
                Err(BlkError::Offline)
            }
        }
    }

    pub(super) fn lifecycle_domain_mut(&mut self) -> Result<&mut ReadyDomain<H>, BlkError> {
        match &mut self.state {
            DomainState::Ready(ready) | DomainState::Failed(Some(ready)) => Ok(ready),
            DomainState::Initial { .. }
            | DomainState::Failed(None)
            | DomainState::Transitioning => Err(BlkError::Offline),
        }
    }
}

impl PreludeSession {
    fn advance(&mut self, now_ns: u64) -> Result<PreludeProgress, InitError> {
        match self.state {
            PreludeState::Prepare => {
                let settle_ns = self.owner.prepare()?;
                if settle_ns == 0 {
                    self.state = PreludeState::Ready;
                    Ok(PreludeProgress::Ready)
                } else {
                    let wake_at_ns = now_ns.saturating_add(settle_ns);
                    self.state = PreludeState::Settling { wake_at_ns };
                    ControlSchedule::new(false, rdif_block::IdList::none(), Some(wake_at_ns))
                        .map(PreludeProgress::Pending)
                }
            }
            PreludeState::Settling { wake_at_ns } if now_ns < wake_at_ns => {
                ControlSchedule::new(false, rdif_block::IdList::none(), Some(wake_at_ns))
                    .map(PreludeProgress::Pending)
            }
            PreludeState::Settling { .. } => {
                self.state = PreludeState::Ready;
                Ok(PreludeProgress::Ready)
            }
            PreludeState::Ready => Ok(PreludeProgress::Ready),
            PreludeState::Failed => Err(InitError::InvalidState),
        }
        .inspect_err(|_| self.state = PreludeState::Failed)
    }
}

impl<H: BlockHost> ReadyDomain<H> {
    fn new(card: crate::sdio::SdioSdmmc<H>, config: BlockConfig) -> Self {
        Self {
            card,
            config,
            queue: SdmmcRequestQueue::new(),
            recovery: RecoveryState::Idle,
            epoch: rdif_block::ControllerEpoch::INITIAL,
            fault: None,
        }
    }

    fn device_info(&self) -> rdif_block::DeviceInfo {
        crate::rdif::device_info(&self.config)
    }

    fn queue_limits(&self) -> HardwareQueueLimits {
        queue_limits(&self.config, self.config.dma_mask).into()
    }
}

impl<H> DriverGeneric for CombinedSdmmcDomain<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
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

impl<H> ControllerControl for CombinedSdmmcDomain<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn controller_identity(&self) -> NonZeroUsize {
        self.identity
    }

    fn service_control(
        &mut self,
        trigger: DriverControlTrigger,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        match trigger {
            DriverControlTrigger::Start { now_ns }
            | DriverControlTrigger::InternalProgress { now_ns }
            | DriverControlTrigger::ProtocolDeadline { now_ns } => {
                self.service_without_irq(now_ns, publication)
            }
            DriverControlTrigger::Irq { now_ns, evidence } => {
                self.service_initial_irq(now_ns, evidence, publication)
            }
            DriverControlTrigger::BeginQuiesce {
                now_ns,
                intent,
                epoch,
            } => self.begin_quiesce(now_ns, intent, epoch),
            DriverControlTrigger::BeginReinitialize { now_ns, quiesced } => {
                self.begin_reinitialize(now_ns, quiesced)
            }
        }
    }

    fn service_ready_evidence(
        &mut self,
        _evidence: IrqEvidenceId,
    ) -> Result<EvidenceServiceResult, BlkError> {
        Err(BlkError::Other(
            "combined SD/MMC evidence must be serviced by its shared I/O owner",
        ))
    }

    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        commit_ledger_evidence(&self.ledger, evidence)
    }

    fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        self.ledger.retire_after_quiesce(permit, self.identity)
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        match &self.state {
            DomainState::Ready(_) | DomainState::Failed(Some(_)) => {
                LifecycleEndpoint::Interrupt(self)
            }
            DomainState::Initial { .. }
            | DomainState::Failed(None)
            | DomainState::Transitioning => LifecycleEndpoint::Inline,
        }
    }

    fn enable_irq(&mut self) -> Result<(), BlkError> {
        self.prelude.irq_requested = true;
        let host = match &mut self.state {
            DomainState::Initial { init, config } => {
                if !config.supports_runtime_queue() {
                    return Err(BlkError::NotSupported);
                }
                if !matches!(self.prelude.state, PreludeState::Ready) {
                    return Ok(());
                }
                init.enable_completion_irq()
                    .map_err(map_dev_err_to_blk_err)?;
                return init
                    .completion_irq_enabled()
                    .then_some(())
                    .ok_or(BlkError::Io);
            }
            DomainState::Ready(ready) => ready.card.host_mut(),
            DomainState::Failed(Some(ready)) => ready.card.host_mut(),
            DomainState::Failed(None) | DomainState::Transitioning => {
                return Err(BlkError::Offline);
            }
        };
        SdioHost::enable_completion_irq(host).map_err(map_dev_err_to_blk_err)?;
        host.completion_irq_enabled()
            .then_some(())
            .ok_or(BlkError::Io)
    }

    fn disable_irq(&mut self) -> Result<(), BlkError> {
        self.prelude.irq_requested = false;
        let host = match &mut self.state {
            DomainState::Initial { init, .. } => {
                init.disable_completion_irq()
                    .map_err(map_dev_err_to_blk_err)?;
                return (!init.completion_irq_enabled())
                    .then_some(())
                    .ok_or(BlkError::Io);
            }
            DomainState::Ready(ready) => ready.card.host_mut(),
            DomainState::Failed(Some(ready)) => ready.card.host_mut(),
            DomainState::Failed(None) | DomainState::Transitioning => {
                return Err(BlkError::Offline);
            }
        };
        SdioHost::disable_completion_irq(host).map_err(map_dev_err_to_blk_err)?;
        (!host.completion_irq_enabled())
            .then_some(())
            .ok_or(BlkError::Io)
    }

    fn is_irq_enabled(&self) -> bool {
        match &self.state {
            DomainState::Initial { init, .. } => init.completion_irq_enabled(),
            DomainState::Ready(ready) => ready.card.host().completion_irq_enabled(),
            DomainState::Failed(Some(ready)) => ready.card.host().completion_irq_enabled(),
            DomainState::Failed(None) | DomainState::Transitioning => false,
        }
    }
}

impl<H> InterruptIoDomain for CombinedSdmmcDomain<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn domain_id(&self) -> OwnershipDomainId {
        self.domain
    }

    fn queue_count(&self) -> usize {
        1
    }

    fn submit_owned(
        &mut self,
        queue_id: usize,
        logical_device: DriverDeviceKey,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<rdif_block::AcceptedRequest, UnacceptedRequest> {
        if queue_id != 0 || logical_device != SDMMC_DRIVER_DEVICE_KEY {
            return Err(UnacceptedRequest::new(
                id,
                BlkError::InvalidRequest,
                request,
            ));
        }
        let ready = match self.ready_mut() {
            Ok(ready) if ready.fault.is_none() => ready,
            _ => return Err(UnacceptedRequest::new(id, BlkError::Offline, request)),
        };
        let device = ready.device_info();
        let limits = ready.queue_limits();
        ready
            .queue
            .submit_owned(&mut ready.card, &ready.config, device, limits, id, request)
    }

    fn service_evidence(
        &mut self,
        evidence: IrqEvidenceId,
        sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError> {
        let ledger = Arc::clone(&self.ledger);
        let batch = match ledger.begin_service(evidence) {
            Ok(batch) => batch,
            Err(_) => return Ok(EvidenceServiceResult::Recover(ControllerFault::Ownership)),
        };
        let facts = batch.facts();
        if facts.has_overflow() {
            let _ = ledger.finish_service(batch, SdmmcIrqFacts::none());
            return Ok(EvidenceServiceResult::Recover(ControllerFault::Protocol));
        }
        if facts.requires_queue_service() {
            let ready = self.ready_mut()?;
            if !ready.queue.has_active_request() {
                let _ = ledger.finish_service(batch, SdmmcIrqFacts::none());
                ready.fault = Some(ControllerFault::Protocol);
                return Ok(EvidenceServiceResult::Recover(ControllerFault::Protocol));
            }
            if ready
                .queue
                .service_evidence(&mut ready.card, facts.snapshot(), sink)
                .is_err()
            {
                let _ = ledger.finish_service(batch, SdmmcIrqFacts::none());
                ready.fault = Some(ControllerFault::Protocol);
                return Ok(EvidenceServiceResult::Recover(ControllerFault::Protocol));
            }
        }
        Ok(evidence_result(
            ledger.finish_service(batch, SdmmcIrqFacts::none()),
        ))
    }

    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        commit_ledger_evidence(&self.ledger, evidence)
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
        let identity = self.identity.get();
        let ready = self.ready_mut()?;
        ready
            .queue
            .reclaim_after_quiesce(&mut ready.card, identity, proof, sink)
    }

    fn resume_after_reinitialize(
        &mut self,
        epoch: rdif_block::ControllerEpoch,
    ) -> Result<(), BlkError> {
        {
            let ready = self.ready_mut()?;
            if epoch <= ready.epoch || !matches!(ready.recovery, RecoveryState::Idle) {
                return Err(BlkError::InvalidDmaProof);
            }
        }
        self.evidence_epoch.advance()?;
        let ready = self.ready_mut()?;
        ready.epoch = epoch;
        ready.fault = None;
        ready.queue.resume();
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        self.ready_mut()?.queue.shutdown()
    }
}

impl<H> SharedControllerIoDomain for CombinedSdmmcDomain<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn io_domain_mut(&mut self) -> &mut dyn InterruptIoDomain {
        self
    }
}

fn evidence_result(disposition: SdmmcEvidenceDisposition) -> EvidenceServiceResult {
    match disposition {
        SdmmcEvidenceDisposition::Drained => EvidenceServiceResult::Drained,
        SdmmcEvidenceDisposition::Retained => EvidenceServiceResult::Retained,
        SdmmcEvidenceDisposition::Invalid => {
            EvidenceServiceResult::Recover(ControllerFault::Ownership)
        }
    }
}

fn commit_ledger_evidence(
    ledger: &SdmmcEvidenceLedger,
    evidence: IrqEvidenceId,
) -> Result<DriverEvidenceRetirement, BlkError> {
    ledger
        .commit_drained_evidence(evidence)
        .map_err(|_| BlkError::Other("SD/MMC evidence commit is invalid"))
}

fn initialization_recovery(fault: ControllerFault) -> DriverControlPoll {
    DriverControlPoll::after_irq(
        ControlProgress::Failed(InitError::Hardware(
            "SD/MMC initialization IRQ evidence is inconsistent",
        )),
        EvidenceServiceResult::Recover(fault),
    )
}

fn lifecycle_schedule(schedule: rdif_block::InitSchedule) -> ControlProgress {
    match ControlSchedule::new(
        schedule.run_again(),
        schedule.irq_sources(),
        schedule.wake_at_ns(),
    ) {
        Ok(schedule) => ControlProgress::Pending(schedule),
        Err(error) => ControlProgress::Failed(error),
    }
}

fn map_init_error(error: crate::Error) -> InitError {
    match error {
        crate::Error::Timeout(_) => InitError::TimedOut,
        crate::Error::InvalidArgument => InitError::InvalidState,
        crate::Error::UnsupportedCommand => {
            InitError::Hardware("SD/MMC host does not support bounded initialization")
        }
        _ => InitError::Hardware("SD/MMC card initialization failed"),
    }
}
