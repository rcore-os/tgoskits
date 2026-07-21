use core::num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use rdif_block::{
    AcceptedRequest, ActivationError, ActivationPlan, BlkError, BlockEvidenceSource,
    CompletedRequest, CompletionSink, ContainmentCause, ControlProgress, ControlTrigger,
    ControllerCapabilities, ControllerControl, ControllerControlPart, ControllerEpoch,
    ControllerFault, ControllerPublicationFactory, ControllerReady, ControllerReinitialized,
    DomainActivationPlan, DomainIrqSource, DomainOwnerBinding, DomainResumeError,
    DriverControlPoll, DriverControlTrigger, DriverDeviceKey, DriverGeneric,
    DriverLogicalDeviceDesc, EvidenceClaim, EvidenceLatch, EvidenceServiceResult,
    HardwareQueueDepth, HardwareQueueLimits, InterruptIoDomain, InterruptQueueDesc, IrqCapture,
    IrqControlError, IrqEndpoint, IrqEventEpoch, IrqEvidenceId, IrqSourceControl, IrqSourceId,
    LifecycleEndpoint, LogicalDeviceCapability, LogicalDeviceConstraints, LogicalDeviceSelector,
    MaskedSource, OwnedRequest, OwnershipDomainCapability, OwnershipDomainId, PendingBlockIrq,
    PreparedControllerParts, PublishedController, QueueExecution, RequestId,
    SharedControllerIoDomain, UnacceptedRequest,
};

struct CombinedSdDomain {
    identity: NonZeroUsize,
    domain: OwnershipDomainId,
    ready: Option<DriverLogicalDeviceDesc>,
    active_epoch: Arc<AtomicU64>,
}

impl DriverGeneric for CombinedSdDomain {
    fn name(&self) -> &str {
        "combined-sd-domain"
    }
}

impl ControllerControl for CombinedSdDomain {
    fn controller_identity(&self) -> NonZeroUsize {
        self.identity
    }

    fn service_control(
        &mut self,
        _trigger: DriverControlTrigger,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        let logical_device = self.ready.take().expect("ready publication is one-shot");
        DriverControlPoll::without_evidence(ControlProgress::PublicationReady(
            publication
                .publish_combined(vec![logical_device], vec![])
                .expect("the combined publication must match the activation plan"),
        ))
    }

    fn service_ready_evidence(
        &mut self,
        _evidence: IrqEvidenceId,
    ) -> Result<EvidenceServiceResult, BlkError> {
        panic!("combined normal-I/O evidence must not be routed through control service")
    }

    fn commit_drained_evidence(
        &mut self,
        _evidence: IrqEvidenceId,
    ) -> Result<rdif_block::DriverEvidenceRetirement, BlkError> {
        Ok(rdif_block::DriverEvidenceRetirement::Retired)
    }

    fn retire_recovery_evidence(
        &mut self,
        permit: rdif_block::RecoveryEvidenceRetirePermit,
    ) -> Result<rdif_block::RecoveryEvidenceRetired, rdif_block::RecoveryEvidenceRetireFailure>
    {
        permit.retire_with(self.identity, |_, _| Ok(()))
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        LifecycleEndpoint::Inline
    }

    fn enable_irq(&mut self) -> Result<(), BlkError> {
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<(), BlkError> {
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        false
    }
}

impl InterruptIoDomain for CombinedSdDomain {
    fn domain_id(&self) -> OwnershipDomainId {
        self.domain
    }

    fn queue_count(&self) -> usize {
        1
    }

    fn submit_owned(
        &mut self,
        _queue_id: usize,
        _logical_device: DriverDeviceKey,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest> {
        Err(UnacceptedRequest::new(id, BlkError::Offline, request))
    }

    fn service_evidence(
        &mut self,
        _evidence: IrqEvidenceId,
        _sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError> {
        Ok(EvidenceServiceResult::Recover(ControllerFault::Protocol))
    }

    fn commit_drained_evidence(
        &mut self,
        _evidence: IrqEvidenceId,
    ) -> Result<rdif_block::DriverEvidenceRetirement, BlkError> {
        Ok(rdif_block::DriverEvidenceRetirement::Retired)
    }

    fn retire_recovery_evidence(
        &mut self,
        permit: rdif_block::RecoveryEvidenceRetirePermit,
    ) -> Result<rdif_block::RecoveryEvidenceRetired, rdif_block::RecoveryEvidenceRetireFailure>
    {
        permit.retire_with(self.identity, |_, _| Ok(()))
    }

    fn reclaim_after_quiesce(
        &mut self,
        _proof: &rdif_block::DmaQuiesced,
        _sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        Ok(())
    }

    fn resume_after_reinitialize(&mut self, epoch: ControllerEpoch) -> Result<(), BlkError> {
        self.active_epoch.store(epoch.get(), Ordering::Release);
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        Ok(())
    }
}

impl SharedControllerIoDomain for CombinedSdDomain {
    fn io_domain_mut(&mut self) -> &mut dyn InterruptIoDomain {
        self
    }
}

struct EmptyEvidence;

impl IrqEndpoint for EmptyEvidence {
    type Event = IrqEvidenceId;
    type Fault = BlkError;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        IrqCapture::Unhandled
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        Err(BlkError::Offline)
    }
}

struct EmptyIrqControl;

impl IrqSourceControl for EmptyIrqControl {
    type Error = IrqControlError;

    fn rearm(&mut self, _source: MaskedSource) -> Result<(), Self::Error> {
        Err(IrqControlError::Offline)
    }
}

struct RejectCompletion;

impl CompletionSink for RejectCompletion {
    fn complete(&mut self, _completion: CompletedRequest) {
        panic!("the fake combined domain does not complete requests")
    }
}

#[test]
fn shared_control_and_io_remain_one_move_only_owner_after_ready() {
    let identity = NonZeroUsize::new(0x5d).unwrap();
    let domain = OwnershipDomainId::new(2).unwrap();
    let active_epoch = Arc::new(AtomicU64::new(ControllerEpoch::INITIAL.get()));
    let source = IrqSourceId::new(2).unwrap();
    let driver_key = DriverDeviceKey::new(NonZeroU64::new(1).unwrap());
    let selector = LogicalDeviceSelector::exact(vec![driver_key]).unwrap();
    let capability = OwnershipDomainCapability::new(
        domain,
        selector.clone(),
        QueueExecution::Serialized,
        NonZeroU16::new(1).unwrap(),
        NonZeroU16::new(1).unwrap(),
        HardwareQueueDepth::fixed(NonZeroU16::new(1).unwrap()),
        rdif_block::IdList::from_bits(1 << source.get()),
    )
    .unwrap();
    let capabilities = ControllerCapabilities::new(
        identity,
        vec![LogicalDeviceCapability::new(
            driver_key,
            LogicalDeviceConstraints::discover_during_init(
                rdif_block::dma_api::DmaDomainId::legacy_global(),
                u64::MAX,
            ),
        )],
        vec![capability],
    )
    .unwrap();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            domain,
            NonZeroU16::new(1).unwrap(),
            NonZeroU16::new(1).unwrap(),
            rdif_block::IdList::from_bits(1 << source.get()),
        )],
    )
    .unwrap();
    let queue = InterruptQueueDesc::new(
        0,
        selector,
        domain,
        QueueExecution::Serialized,
        NonZeroU16::new(1).unwrap(),
        rdif_block::IdList::from_bits(1 << source.get()),
    )
    .unwrap();
    let logical_device = DriverLogicalDeviceDesc::new(
        driver_key,
        "sd0",
        rdif_block::DeviceInfo::new(4096, 512),
        HardwareQueueLimits::simple(512, u64::MAX),
    );
    let mut control = ControllerControlPart::new_combined_shared(
        domain,
        vec![DomainIrqSource::new(
            source,
            BlockEvidenceSource::new(Box::new(EmptyEvidence), Box::new(EmptyIrqControl)),
        )],
        vec![queue],
        Box::new(CombinedSdDomain {
            identity,
            domain,
            ready: Some(logical_device),
            active_epoch,
        }),
    )
    .unwrap();
    for source in control.owned_irq_sources_mut() {
        let registration = source.take_for_registration().unwrap();
        source.finish_registration().unwrap();
        drop(registration);
    }
    let mut prepared = PreparedControllerParts::new(plan, control).unwrap();
    let ControlProgress::PublicationReady(ready) = prepared
        .service_control(ControlTrigger::Start { now_ns: 1 })
        .unwrap()
        .into_parts()
        .0
    else {
        panic!("combined controller must publish after initialization")
    };

    let staged = prepared.stage(ready).unwrap();
    let (mut coordinator, domains) = staged.into_installations();
    assert!(
        domains.is_empty(),
        "the shared queue owner must not be duplicated as an unbound domain"
    );
    let owner = DomainOwnerBinding::new(0, NonZeroU64::new(0x77).unwrap());
    coordinator.bind_combined_control_domain(owner).unwrap();
    let mut published = coordinator.publish().unwrap();

    assert_eq!(published.bound_domains().len(), 1);
    assert_eq!(published.bound_domains()[0].owner(), owner);
    assert_eq!(published.shared_io_queues().unwrap().len(), 1);
    assert_eq!(
        published
            .shared_io_domain_mut()
            .expect("combined owner must remain in the control session")
            .domain_id(),
        domain
    );

    let latch = Box::pin(EvidenceLatch::new(source));
    let evidence = IrqEvidenceId::new(
        source,
        NonZeroU64::new(1).unwrap(),
        0,
        NonZeroU32::new(1).unwrap(),
    );
    let EvidenceClaim::Claimed(claim) = latch.as_ref().claim(evidence, None).unwrap() else {
        panic!("an idle source latch must mint one linear claim")
    };
    let pending = PendingBlockIrq::from_claim(claim, IrqEventEpoch::new(9).unwrap());

    let failure = published
        .control_mut()
        .service_evidence(pending)
        .unwrap_err();
    let (error, retained) = failure.into_parts();
    assert!(matches!(error, BlkError::Other(_)));
    assert_eq!(retained.evidence_id(), evidence);
    assert_eq!(retained.source_epoch().get(), 9);

    let mut sink = RejectCompletion;
    assert_eq!(
        published
            .shared_io_domain_mut()
            .unwrap()
            .service_evidence(evidence, &mut sink),
        Ok(EvidenceServiceResult::Recover(ControllerFault::Protocol))
    );
}

#[test]
fn combined_owner_consumes_only_bound_newer_reinitialization_permits() {
    let identity = NonZeroUsize::new(0x5d).unwrap();
    let domain = OwnershipDomainId::new(2).unwrap();
    let (mut published, active_epoch) = publish_combined_domain(identity, domain);

    assert_eq!(published.shared_io_epoch(), Some(ControllerEpoch::INITIAL));
    assert_eq!(
        published.active_controller_epoch(),
        ControllerEpoch::INITIAL
    );

    let next_epoch = ControllerEpoch::new(2);
    let (mut pending_commit, permit) = bound_permit(&published, identity, domain, next_epoch);
    let resumed = published
        .resume_shared_io_after_reinitialize(permit)
        .unwrap();
    assert_eq!(published.shared_io_epoch(), Some(next_epoch));
    assert_eq!(active_epoch.load(Ordering::Acquire), next_epoch.get());
    assert_eq!(
        published.active_controller_epoch(),
        ControllerEpoch::INITIAL,
        "the global epoch stays old until the resumed proof is committed"
    );
    pending_commit.accept_resumed(resumed).unwrap();
    let commit = pending_commit.finish().unwrap();
    assert_eq!(
        published.commit_reinitialized_epoch(commit).unwrap(),
        next_epoch
    );

    // SAFETY: the fake has no DMA and retains the exact controller identity.
    let stale_ready = unsafe { ControllerReady::new(next_epoch, identity.get()) };
    let stale = ControllerReinitialized::new(stale_ready, vec![domain]).unwrap();
    let failure = published.control().bind_reinitialized(stale).unwrap_err();
    assert_eq!(
        failure.error(),
        &ActivationError::ReinitEpochDidNotAdvance {
            active: next_epoch,
            captured: next_epoch,
        }
    );

    let wrong_domain = OwnershipDomainId::new(3).unwrap();
    // SAFETY: the fake has no DMA and retains the exact controller identity.
    let wrong_ready = unsafe { ControllerReady::new(ControllerEpoch::new(3), identity.get()) };
    let wrong = ControllerReinitialized::new(wrong_ready, vec![wrong_domain]).unwrap();
    let failure = published.control().bind_reinitialized(wrong).unwrap_err();
    assert_eq!(failure.error(), &ActivationError::ReinitPermitSetMismatch);

    let (other_publication, _) = publish_combined_domain(identity, domain);
    let (_foreign_commit, foreign_seal) = bound_permit(
        &other_publication,
        identity,
        domain,
        ControllerEpoch::new(3),
    );
    let failure = published
        .resume_shared_io_after_reinitialize(foreign_seal)
        .unwrap_err();
    let (error, retained) = failure.into_parts();
    assert_eq!(error, DomainResumeError::ForeignPermit);
    assert_eq!(retained.controller_identity(), identity);

    let foreign_identity = NonZeroUsize::new(0x6d).unwrap();
    let (other_controller, _) = publish_combined_domain(foreign_identity, domain);
    let (_foreign_commit, foreign_controller) = bound_permit(
        &other_controller,
        foreign_identity,
        domain,
        ControllerEpoch::new(3),
    );
    let failure = published
        .resume_shared_io_after_reinitialize(foreign_controller)
        .unwrap_err();
    let (error, retained) = failure.into_parts();
    assert_eq!(error, DomainResumeError::ForeignPermit);
    assert_eq!(retained.controller_identity(), foreign_identity);
}

#[test]
fn epoch_commit_rejects_a_predecessor_that_is_no_longer_active() {
    let identity = NonZeroUsize::new(0x7d).unwrap();
    let domain = OwnershipDomainId::new(2).unwrap();
    let (mut published, _) = publish_combined_domain(identity, domain);

    let (mut pending_two, permit_two) =
        bound_permit(&published, identity, domain, ControllerEpoch::new(2));
    let resumed_two = published
        .resume_shared_io_after_reinitialize(permit_two)
        .unwrap();
    pending_two.accept_resumed(resumed_two).unwrap();
    let commit_two = pending_two.finish().unwrap();

    let (mut pending_three, permit_three) =
        bound_permit(&published, identity, domain, ControllerEpoch::new(3));
    let resumed_three = published
        .resume_shared_io_after_reinitialize(permit_three)
        .unwrap();
    pending_three.accept_resumed(resumed_three).unwrap();
    let commit_three = pending_three.finish().unwrap();

    assert_eq!(
        published.commit_reinitialized_epoch(commit_three).unwrap(),
        ControllerEpoch::new(3)
    );
    let failure = published
        .commit_reinitialized_epoch(commit_two)
        .unwrap_err();
    assert_eq!(
        failure.error(),
        rdif_block::ControllerEpochCommitError::ActiveEpochChanged {
            expected: ControllerEpoch::INITIAL,
            active: ControllerEpoch::new(3),
        }
    );
    let (_, retained) = failure.into_parts();
    assert_eq!(retained.epoch(), ControllerEpoch::new(2));
}

#[test]
fn resume_collector_rejects_foreign_proof_without_losing_its_owner() {
    let identity = NonZeroUsize::new(0x8d).unwrap();
    let domain = OwnershipDomainId::new(2).unwrap();
    let (published, _) = publish_combined_domain(identity, domain);
    let (mut pending, _local_permit) =
        bound_permit(&published, identity, domain, ControllerEpoch::new(2));

    let (mut foreign_publication, _) = publish_combined_domain(identity, domain);
    let (_foreign_pending, foreign_permit) = bound_permit(
        &foreign_publication,
        identity,
        domain,
        ControllerEpoch::new(2),
    );
    let foreign_resumed = foreign_publication
        .resume_shared_io_after_reinitialize(foreign_permit)
        .unwrap();

    let failure = pending.accept_resumed(foreign_resumed).unwrap_err();
    assert_eq!(
        failure.error(),
        rdif_block::ControllerEpochCommitError::ForeignPublication
    );
    let (_, retained) = failure.into_parts();
    assert_eq!(retained.domain(), domain);
    assert_eq!(retained.epoch(), ControllerEpoch::new(2));
}

fn publish_combined_domain(
    identity: NonZeroUsize,
    domain: OwnershipDomainId,
) -> (PublishedController, Arc<AtomicU64>) {
    let source = IrqSourceId::new(2).unwrap();
    let driver_key = DriverDeviceKey::new(NonZeroU64::new(1).unwrap());
    let selector = LogicalDeviceSelector::exact(vec![driver_key]).unwrap();
    let capabilities = ControllerCapabilities::new(
        identity,
        vec![LogicalDeviceCapability::new(
            driver_key,
            LogicalDeviceConstraints::discover_during_init(
                rdif_block::dma_api::DmaDomainId::legacy_global(),
                u64::MAX,
            ),
        )],
        vec![
            OwnershipDomainCapability::new(
                domain,
                selector.clone(),
                QueueExecution::Serialized,
                NonZeroU16::new(1).unwrap(),
                NonZeroU16::new(1).unwrap(),
                HardwareQueueDepth::fixed(NonZeroU16::new(1).unwrap()),
                rdif_block::IdList::from_bits(1 << source.get()),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            domain,
            NonZeroU16::new(1).unwrap(),
            NonZeroU16::new(1).unwrap(),
            rdif_block::IdList::from_bits(1 << source.get()),
        )],
    )
    .unwrap();
    let queue = InterruptQueueDesc::new(
        0,
        selector,
        domain,
        QueueExecution::Serialized,
        NonZeroU16::new(1).unwrap(),
        rdif_block::IdList::from_bits(1 << source.get()),
    )
    .unwrap();
    let logical_device = DriverLogicalDeviceDesc::new(
        driver_key,
        "sd0",
        rdif_block::DeviceInfo::new(4096, 512),
        HardwareQueueLimits::simple(512, u64::MAX),
    );
    let active_epoch = Arc::new(AtomicU64::new(ControllerEpoch::INITIAL.get()));
    let mut control = ControllerControlPart::new_combined_shared(
        domain,
        vec![DomainIrqSource::new(
            source,
            BlockEvidenceSource::new(Box::new(EmptyEvidence), Box::new(EmptyIrqControl)),
        )],
        vec![queue],
        Box::new(CombinedSdDomain {
            identity,
            domain,
            ready: Some(logical_device),
            active_epoch: Arc::clone(&active_epoch),
        }),
    )
    .unwrap();
    for source in control.owned_irq_sources_mut() {
        let registration = source.take_for_registration().unwrap();
        source.finish_registration().unwrap();
        drop(registration);
    }
    let mut prepared = PreparedControllerParts::new(plan, control).unwrap();
    let ControlProgress::PublicationReady(ready) = prepared
        .service_control(ControlTrigger::Start { now_ns: 1 })
        .unwrap()
        .into_parts()
        .0
    else {
        panic!("combined controller must publish after initialization")
    };
    let staged = prepared.stage(ready).unwrap();
    let (mut coordinator, domains) = staged.into_installations();
    assert!(domains.is_empty());
    coordinator
        .bind_combined_control_domain(DomainOwnerBinding::new(0, NonZeroU64::new(0x77).unwrap()))
        .unwrap();
    (coordinator.publish().unwrap(), active_epoch)
}

fn bound_permit(
    published: &PublishedController,
    identity: NonZeroUsize,
    domain: OwnershipDomainId,
    epoch: ControllerEpoch,
) -> (
    rdif_block::PendingControllerEpochCommit,
    rdif_block::DomainReinitPermit,
) {
    // SAFETY: the fake owns no DMA and keeps this controller identity stable
    // for the complete publication lifetime.
    let ready = unsafe { ControllerReady::new(epoch, identity.get()) };
    let reinitialized = ControllerReinitialized::new(ready, vec![domain]).unwrap();
    let bound = published
        .control()
        .bind_reinitialized(reinitialized)
        .unwrap();
    let (pending_commit, mut permits) = bound.into_resume_parts();
    (pending_commit, permits.pop().unwrap())
}
