use core::num::{NonZeroU16, NonZeroU64, NonZeroUsize};

use rdif_block::{
    AcceptedRequest, ActivationError, ActivationPlan, BlkError, BlockEvidenceSource,
    CompletionSink, ContainmentCause, ControlProgress, ControlTrigger, ControllerCapabilities,
    ControllerControl, ControllerControlPart, ControllerEpoch, ControllerFault,
    ControllerPublicationFactory, ControllerPublicationReady, ControllerReady,
    ControllerReinitialized, DeviceInfo, DmaQuiesced, DomainActivationPlan, DomainIrqSource,
    DomainOwnerBinding, DriverControlPoll, DriverControlTrigger, DriverDeviceKey, DriverGeneric,
    DriverLogicalDeviceDesc, EvidenceServiceResult, HardwareQueueDepth, HardwareQueueLimits,
    InstalledIoDomain, InterruptIoDomain, InterruptQueueDesc, IoDomainIrqSource, IoDomainPart,
    IrqCapture, IrqControlError, IrqEndpoint, IrqEvidenceId, IrqSourceControl, IrqSourceId,
    LifecycleEndpoint, LogicalDeviceCapability, LogicalDeviceConstraints, LogicalDeviceSelector,
    MaskedSource, OwnedRequest, OwnershipDomainCapability, OwnershipDomainId, OwnershipDomainIds,
    PreparedControllerParts, PublishedController, QueueExecution, RequestId, UnacceptedRequest,
};

struct ReadyControl {
    identity: NonZeroUsize,
    ready: Option<Vec<DriverLogicalDeviceDesc>>,
    selectors: Vec<LogicalDeviceSelector>,
}

impl DriverGeneric for ReadyControl {
    fn name(&self) -> &str {
        "ready-control"
    }
}

impl ControllerControl for ReadyControl {
    fn controller_identity(&self) -> NonZeroUsize {
        self.identity
    }

    fn service_control(
        &mut self,
        trigger: DriverControlTrigger,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        match trigger {
            DriverControlTrigger::Start { .. } => {
                let logical_devices = self.ready.take().expect("ready proof is one-shot");
                DriverControlPoll::without_evidence(ControlProgress::PublicationReady(
                    publication
                        .publish(logical_devices, vec![io_domain(self.selectors.clone())])
                        .unwrap(),
                ))
            }
            DriverControlTrigger::BeginQuiesce { epoch, .. } => {
                // SAFETY: this deterministic fake has no DMA and returns the
                // stable controller identity retained by this test owner.
                let proof = unsafe { DmaQuiesced::new(epoch, self.identity.get()) };
                DriverControlPoll::without_evidence(ControlProgress::DmaQuiesced(proof))
            }
            DriverControlTrigger::BeginReinitialize { quiesced, .. } => {
                if quiesced.controller_cookie() != self.identity.get() {
                    return DriverControlPoll::without_evidence(ControlProgress::Failed(
                        rdif_block::InitError::InvalidState,
                    ));
                }
                // SAFETY: the fake has no DMA or hardware state and consumes
                // the exact proof produced by its preceding quiesce pass.
                let ready =
                    unsafe { ControllerReady::new(quiesced.epoch(), quiesced.controller_cookie()) };
                let reinitialized =
                    ControllerReinitialized::new(ready, vec![OwnershipDomainId::new(2).unwrap()])
                        .unwrap();
                DriverControlPoll::without_evidence(ControlProgress::Reinitialized(reinitialized))
            }
            _ => panic!("ready-control received an unsupported test transition"),
        }
    }

    fn service_ready_evidence(
        &mut self,
        _evidence: IrqEvidenceId,
    ) -> Result<EvidenceServiceResult, BlkError> {
        Ok(EvidenceServiceResult::Drained)
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

struct EmptyDomain {
    id: OwnershipDomainId,
    queue_count: usize,
}

impl InterruptIoDomain for EmptyDomain {
    fn domain_id(&self) -> OwnershipDomainId {
        self.id
    }

    fn queue_count(&self) -> usize {
        self.queue_count
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
        let owner = NonZeroUsize::new(permit.controller_identity())
            .expect("RDIF recovery permits always carry a nonzero owner");
        permit.retire_with(owner, |_, _| Ok(()))
    }

    fn reclaim_after_quiesce(
        &mut self,
        _proof: &DmaQuiesced,
        _sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        Ok(())
    }

    fn resume_after_reinitialize(&mut self, _epoch: ControllerEpoch) -> Result<(), BlkError> {
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        Ok(())
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

#[test]
fn discovery_does_not_require_final_capacity_or_block_size() {
    let logical = LogicalDeviceCapability::new(
        driver_key(0),
        LogicalDeviceConstraints::discover_during_init(
            rdif_block::dma_api::DmaDomainId::legacy_global(),
            u64::MAX,
        ),
    );
    let capabilities = controller_capabilities(logical);

    assert_eq!(
        capabilities.logical_devices()[0].driver_key(),
        driver_key(0)
    );
}

#[test]
fn final_owner_publishes_geometry_once_after_irq_driven_init_is_ready() {
    let identity = NonZeroUsize::new(0x44).unwrap();
    let logical = LogicalDeviceCapability::new(
        driver_key(0),
        LogicalDeviceConstraints::discover_during_init(
            rdif_block::dma_api::DmaDomainId::legacy_global(),
            u64::MAX,
        ),
    );
    let capabilities = controller_capabilities_with_identity(identity, logical);
    let plan = activation_plan(&capabilities);
    let final_device = DriverLogicalDeviceDesc::new(
        driver_key(0),
        "nvme0n1",
        DeviceInfo::new(1_048_576, 4096),
        HardwareQueueLimits::simple(4096, u64::MAX),
    );
    let mut control = ControllerControlPart::new_shared(
        OwnershipDomainId::new(2).unwrap(),
        vec![admin_source(2)],
        Box::new(ReadyControl {
            identity,
            ready: Some(vec![final_device]),
            selectors: vec![LogicalDeviceSelector::exact(vec![driver_key(0)]).unwrap()],
        }),
    )
    .unwrap();
    bind_control_sources(&mut control);
    let mut prepared = PreparedControllerParts::new(plan, control).unwrap();

    let ControlProgress::PublicationReady(ready) = prepared
        .service_control(ControlTrigger::Start { now_ns: 100 })
        .unwrap()
        .into_parts()
        .0
    else {
        panic!("fake controller must complete initialization")
    };
    let (mut activated, mut installed) = finish_staged(prepared, ready);

    assert_eq!(
        activated.logical_devices()[0].device().num_blocks,
        1_048_576
    );
    assert_eq!(
        activated.logical_devices()[0].device().logical_block_size,
        4096
    );
    assert_eq!(activated.logical_devices()[0].driver_key(), driver_key(0));
    assert_eq!(activated.logical_device_routes()[0].queues().bits(), 1);
    assert!(installed.belongs_to(activated.control()));

    let (progress, evidence) = activated
        .control_mut()
        .service_control(ControlTrigger::BeginQuiesce {
            now_ns: 200,
            intent: rdif_block::QuiesceIntent::Shutdown,
            epoch: ControllerEpoch::new(2),
        })
        .unwrap()
        .into_parts();
    let ControlProgress::DmaQuiesced(quiesced) = progress else {
        panic!("fake controller must return the runtime-selected quiesce epoch")
    };
    assert!(evidence.is_none());

    let ControlProgress::Reinitialized(reinitialized) = activated
        .control_mut()
        .service_control(ControlTrigger::BeginReinitialize {
            now_ns: 201,
            quiesced,
        })
        .unwrap()
        .into_parts()
        .0
    else {
        panic!("the quiesce proof must be consumed into a reinitialization proof")
    };

    // SAFETY: this deterministic fake has no DMA and uses the control identity
    // as its stable controller cookie for the whole test lifetime.
    let unbound_ready = unsafe { ControllerReady::new(ControllerEpoch::new(2), identity.get()) };
    let unbound_reinit =
        ControllerReinitialized::new(unbound_ready, vec![OwnershipDomainId::new(2).unwrap()])
            .unwrap();
    let (_, mut unbound_permits) = unbound_reinit.into_parts();
    let failure = installed
        .resume_after_reinitialize(unbound_permits.pop().unwrap())
        .unwrap_err();
    assert_eq!(
        failure.error(),
        rdif_block::DomainResumeError::ForeignPermit
    );

    let bound_reinit = activated
        .control()
        .bind_reinitialized(reinitialized)
        .unwrap();
    let (pending_commit, mut bound_permits) = bound_reinit.into_resume_parts();
    let incomplete = pending_commit.finish().unwrap_err();
    assert_eq!(
        incomplete.error(),
        rdif_block::ControllerEpochCommitError::MissingDomain {
            domain: OwnershipDomainId::new(2).unwrap(),
        }
    );
    let (_, mut pending_commit) = incomplete.into_parts();
    let resumed = installed
        .resume_after_reinitialize(bound_permits.pop().unwrap())
        .unwrap();
    assert_eq!(
        activated.active_controller_epoch(),
        ControllerEpoch::INITIAL,
        "binding and resuming one domain must not publish the controller epoch"
    );
    pending_commit.accept_resumed(resumed).unwrap();
    let commit = pending_commit.finish().unwrap();
    assert_eq!(
        activated.commit_reinitialized_epoch(commit).unwrap(),
        ControllerEpoch::new(2)
    );
    assert_eq!(activated.active_controller_epoch(), ControllerEpoch::new(2));
}

#[test]
fn discover_contract_allows_a_ready_controller_with_no_namespace() {
    let identity = NonZeroUsize::new(0x55).unwrap();
    let capabilities = discovering_capabilities(identity, NonZeroU16::new(4).unwrap());
    let plan = activation_plan(&capabilities);
    let mut control = ControllerControlPart::new_shared(
        OwnershipDomainId::new(2).unwrap(),
        vec![admin_source(2)],
        Box::new(ReadyControl {
            identity,
            ready: Some(Vec::new()),
            selectors: vec![LogicalDeviceSelector::AllPublished],
        }),
    )
    .unwrap();
    bind_control_sources(&mut control);
    let mut prepared = PreparedControllerParts::new(plan, control).unwrap();

    let ControlProgress::PublicationReady(ready) = prepared
        .service_control(ControlTrigger::Start { now_ns: 1 })
        .unwrap()
        .into_parts()
        .0
    else {
        panic!("controller must publish its empty ready namespace set")
    };
    let (activated, _) = finish_staged(prepared, ready);

    assert!(activated.logical_devices().is_empty());
    assert!(activated.logical_device_routes().is_empty());
}

#[test]
fn realized_physical_queue_can_be_explicitly_unrouted() {
    let domain = OwnershipDomainId::new(2).unwrap();
    let source = IrqSourceId::new(2).unwrap();
    let queue = InterruptQueueDesc::new(
        1,
        LogicalDeviceSelector::Unrouted,
        domain,
        QueueExecution::Serialized,
        NonZeroU16::new(1).unwrap(),
        rdif_block::IdList::from_bits(1 << source.get()),
    )
    .unwrap();

    assert!(!queue.logical_devices().contains(driver_key(0)));
}

#[test]
fn ownership_domain_capability_cannot_be_unrouted() {
    let domain = OwnershipDomainId::new(2).unwrap();
    let error = OwnershipDomainCapability::new(
        domain,
        LogicalDeviceSelector::Unrouted,
        QueueExecution::Serialized,
        NonZeroU16::new(1).unwrap(),
        NonZeroU16::new(2).unwrap(),
        HardwareQueueDepth::fixed(NonZeroU16::new(1).unwrap()),
        rdif_block::IdList::from_bits(1 << 2),
    )
    .unwrap_err();

    assert_eq!(error, ActivationError::EmptyDomainDeviceSet { domain });
}

#[test]
fn publication_routes_only_populated_physical_queues() {
    let identity = NonZeroUsize::new(0x56).unwrap();
    let domain = OwnershipDomainId::new(2).unwrap();
    let source_set = rdif_block::IdList::from_bits(1 << 2);
    let capabilities = ControllerCapabilities::new_discovering(
        identity,
        rdif_block::ControlDomainCapability::shared_with_io(domain, source_set).unwrap(),
        NonZeroU16::new(2).unwrap(),
        LogicalDeviceConstraints::discover_during_init(
            rdif_block::dma_api::DmaDomainId::legacy_global(),
            u64::MAX,
        ),
        OwnershipDomainIds::from_bits(1 << domain.get()),
        vec![
            OwnershipDomainCapability::new(
                domain,
                LogicalDeviceSelector::AllPublished,
                QueueExecution::Tagged,
                NonZeroU16::new(2).unwrap(),
                NonZeroU16::new(2).unwrap(),
                HardwareQueueDepth::fixed(NonZeroU16::new(8).unwrap()),
                source_set,
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            domain,
            NonZeroU16::new(2).unwrap(),
            NonZeroU16::new(8).unwrap(),
            source_set,
        )],
    )
    .unwrap();
    let mut control = ControllerControlPart::new_shared(
        domain,
        vec![admin_source(2)],
        Box::new(ReadyControl {
            identity,
            ready: Some(vec![driver_device(driver_key(0), "sda")]),
            selectors: vec![
                LogicalDeviceSelector::exact(vec![driver_key(0)]).unwrap(),
                LogicalDeviceSelector::Unrouted,
            ],
        }),
    )
    .unwrap();
    bind_control_sources(&mut control);
    let mut prepared = PreparedControllerParts::new(plan, control).unwrap();

    let ControlProgress::PublicationReady(ready) = prepared
        .service_control(ControlTrigger::Start { now_ns: 1 })
        .unwrap()
        .into_parts()
        .0
    else {
        panic!("controller must publish its realized port topology")
    };
    let (published, installed) = finish_staged(prepared, ready);

    assert_eq!(published.logical_device_routes()[0].queues().bits(), 1);
    assert_eq!(installed.queues().len(), 2);
    assert!(matches!(
        installed.queues()[1].logical_devices(),
        LogicalDeviceSelector::Unrouted
    ));
}

#[test]
fn invalid_io_domain_returns_queues_sources_and_driver_owner() {
    let domain = OwnershipDomainId::new(2).unwrap();
    let wrong_domain = OwnershipDomainId::new(3).unwrap();
    let source = IrqSourceId::new(2).unwrap();
    let queue = InterruptQueueDesc::new(
        0,
        LogicalDeviceSelector::exact(vec![driver_key(0)]).unwrap(),
        wrong_domain,
        QueueExecution::Tagged,
        NonZeroU16::new(8).unwrap(),
        rdif_block::IdList::from_bits(1 << source.get()),
    )
    .unwrap();
    let failure = IoDomainPart::new(
        domain,
        vec![queue],
        vec![IoDomainIrqSource::New(admin_source(2))],
        Box::new(EmptyDomain {
            id: domain,
            queue_count: 1,
        }),
    )
    .unwrap_err();

    assert_eq!(
        failure.error(),
        &ActivationError::QueueOwnershipMismatch {
            domain,
            queue_id: 0,
        }
    );
    let (error, retained_domain, retained_queues, retained_sources, retained_driver) =
        failure.into_parts();
    assert_eq!(
        error,
        ActivationError::QueueOwnershipMismatch {
            domain,
            queue_id: 0,
        }
    );
    assert_eq!(retained_domain, domain);
    assert_eq!(retained_queues.len(), 1);
    assert_eq!(retained_sources.len(), 1);
    assert_eq!(retained_driver.domain_id(), domain);
}

#[test]
fn sparse_driver_keys_receive_compact_runtime_ids_only_at_finalize() {
    let identity = NonZeroUsize::new(0x66).unwrap();
    let capabilities = discovering_capabilities(identity, NonZeroU16::new(2).unwrap());
    let plan = activation_plan(&capabilities);
    let high_key = DriverDeviceKey::new(NonZeroU64::new(u32::MAX as u64).unwrap());
    let devices = vec![
        driver_device(high_key, "nvme0n4294967295"),
        driver_device(driver_key(6), "nvme0n7"),
    ];
    let mut control = ControllerControlPart::new_shared(
        OwnershipDomainId::new(2).unwrap(),
        vec![admin_source(2)],
        Box::new(ReadyControl {
            identity,
            ready: Some(devices),
            selectors: vec![LogicalDeviceSelector::AllPublished],
        }),
    )
    .unwrap();
    bind_control_sources(&mut control);
    let mut prepared = PreparedControllerParts::new(plan, control).unwrap();

    let ControlProgress::PublicationReady(ready) = prepared
        .service_control(ControlTrigger::Start { now_ns: 1 })
        .unwrap()
        .into_parts()
        .0
    else {
        panic!("controller must publish discovered namespaces")
    };
    let (activated, _) = finish_staged(prepared, ready);

    assert_eq!(activated.logical_devices()[0].id().get(), 0);
    assert_eq!(activated.logical_devices()[0].driver_key(), driver_key(6));
    assert_eq!(activated.logical_devices()[1].id().get(), 1);
    assert_eq!(activated.logical_devices()[1].driver_key(), high_key);
}

fn controller_capabilities(logical: LogicalDeviceCapability) -> ControllerCapabilities {
    controller_capabilities_with_identity(NonZeroUsize::new(0x33).unwrap(), logical)
}

fn controller_capabilities_with_identity(
    identity: NonZeroUsize,
    logical: LogicalDeviceCapability,
) -> ControllerCapabilities {
    ControllerCapabilities::new(
        identity,
        vec![logical],
        vec![
            OwnershipDomainCapability::new(
                OwnershipDomainId::new(2).unwrap(),
                LogicalDeviceSelector::exact(vec![driver_key(0)]).unwrap(),
                QueueExecution::Tagged,
                NonZeroU16::new(1).unwrap(),
                NonZeroU16::new(1).unwrap(),
                HardwareQueueDepth::fixed(NonZeroU16::new(8).unwrap()),
                rdif_block::IdList::from_bits(1 << 2),
            )
            .unwrap(),
        ],
    )
    .unwrap()
}

fn discovering_capabilities(
    identity: NonZeroUsize,
    max_devices: NonZeroU16,
) -> ControllerCapabilities {
    ControllerCapabilities::new_discovering(
        identity,
        rdif_block::ControlDomainCapability::shared_with_io(
            OwnershipDomainId::new(2).unwrap(),
            rdif_block::IdList::from_bits(1 << 2),
        )
        .unwrap(),
        max_devices,
        LogicalDeviceConstraints::discover_during_init(
            rdif_block::dma_api::DmaDomainId::legacy_global(),
            u64::MAX,
        ),
        OwnershipDomainIds::from_bits(1 << 2),
        vec![
            OwnershipDomainCapability::new(
                OwnershipDomainId::new(2).unwrap(),
                LogicalDeviceSelector::AllPublished,
                QueueExecution::Tagged,
                NonZeroU16::new(1).unwrap(),
                NonZeroU16::new(1).unwrap(),
                HardwareQueueDepth::fixed(NonZeroU16::new(8).unwrap()),
                rdif_block::IdList::from_bits(1 << 2),
            )
            .unwrap(),
        ],
    )
    .unwrap()
}

fn driver_device(key: DriverDeviceKey, name: &'static str) -> DriverLogicalDeviceDesc {
    DriverLogicalDeviceDesc::new(
        key,
        name,
        DeviceInfo::new(1_048_576, 4096),
        HardwareQueueLimits::simple(4096, u64::MAX),
    )
}

fn activation_plan(capabilities: &ControllerCapabilities) -> ActivationPlan {
    ActivationPlan::new(
        capabilities,
        vec![DomainActivationPlan::new(
            OwnershipDomainId::new(2).unwrap(),
            NonZeroU16::new(1).unwrap(),
            NonZeroU16::new(8).unwrap(),
            rdif_block::IdList::from_bits(1 << 2),
        )],
    )
    .unwrap()
}

fn finish_staged(
    prepared: PreparedControllerParts,
    ready: ControllerPublicationReady,
) -> (PublishedController, InstalledIoDomain) {
    let staged = prepared.stage(ready).unwrap();
    let (mut coordinator, mut domains) = staged.into_installations();
    assert_eq!(domains.len(), 1);
    let unbound = domains.pop().unwrap();
    let (installed, proof) = unbound
        .finish_binding(DomainOwnerBinding::new(0, NonZeroU64::new(0x55).unwrap()))
        .unwrap();
    coordinator.accept_bound_domain(proof).unwrap();
    (coordinator.publish().unwrap(), installed)
}

fn io_domain(selectors: Vec<LogicalDeviceSelector>) -> IoDomainPart {
    let domain = OwnershipDomainId::new(2).unwrap();
    let source = IrqSourceId::new(2).unwrap();
    let queue_count = selectors.len();
    let queues = selectors
        .into_iter()
        .enumerate()
        .map(|(queue_id, selector)| {
            InterruptQueueDesc::new(
                queue_id,
                selector,
                domain,
                QueueExecution::Tagged,
                NonZeroU16::new(8).unwrap(),
                rdif_block::IdList::from_bits(1 << source.get()),
            )
            .unwrap()
        })
        .collect();
    IoDomainPart::new(
        domain,
        queues,
        vec![IoDomainIrqSource::AlreadyBound(source)],
        Box::new(EmptyDomain {
            id: domain,
            queue_count,
        }),
    )
    .unwrap()
}

fn admin_source(id: usize) -> DomainIrqSource {
    DomainIrqSource::new(
        IrqSourceId::new(id).unwrap(),
        BlockEvidenceSource::new(Box::new(EmptyEvidence), Box::new(EmptyIrqControl)),
    )
}

fn driver_key(id: usize) -> DriverDeviceKey {
    DriverDeviceKey::new(NonZeroU64::new(id as u64 + 1).unwrap())
}

fn bind_control_sources(control: &mut ControllerControlPart) {
    for source in control.owned_irq_sources_mut() {
        let registration = source.take_for_registration().unwrap();
        source.finish_registration().unwrap();
        drop(registration);
    }
}
