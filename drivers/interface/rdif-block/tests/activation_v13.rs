use core::num::{NonZeroU16, NonZeroU64, NonZeroUsize};

use rdif_block::{
    ActivationError, ActivationPlan, BlkError, BlockEvidenceSource, ContainmentCause,
    ControlDomainActivation, ControlDomainCapability, ControlIrqOwnership, ControllerCapabilities,
    ControllerControl, ControllerControlPart, ControllerPublicationFactory, DomainActivationPlan,
    DomainIrqSource, DriverControlPoll, DriverControlTrigger, DriverDeviceKey, DriverGeneric,
    HardwareQueueDepth, InitError, IrqCapture, IrqControlError, IrqEndpoint, IrqEvidenceId,
    IrqSourceControl, IrqSourceId, LifecycleEndpoint, LogicalDeviceCapability,
    LogicalDeviceConstraints, LogicalDeviceSelector, MaskedSource, OwnershipDomainCapability,
    OwnershipDomainId, PreparedControllerParts, QueueExecution,
};

struct TestControl {
    identity: NonZeroUsize,
}

impl DriverGeneric for TestControl {
    fn name(&self) -> &str {
        "test-control"
    }
}

impl ControllerControl for TestControl {
    fn controller_identity(&self) -> NonZeroUsize {
        self.identity
    }

    fn service_control(
        &mut self,
        _trigger: DriverControlTrigger,
        _publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        DriverControlPoll::without_evidence(rdif_block::ControlProgress::Failed(
            InitError::InvalidState,
        ))
    }

    fn service_ready_evidence(
        &mut self,
        _evidence: IrqEvidenceId,
    ) -> Result<rdif_block::EvidenceServiceResult, BlkError> {
        Err(BlkError::NotSupported)
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

struct TestEvidenceEndpoint;

impl IrqEndpoint for TestEvidenceEndpoint {
    type Event = rdif_block::IrqEvidenceId;
    type Fault = BlkError;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        IrqCapture::Unhandled
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        Err(BlkError::Offline)
    }
}

struct TestEvidenceControl;

impl IrqSourceControl for TestEvidenceControl {
    type Error = IrqControlError;

    fn rearm(&mut self, _source: MaskedSource) -> Result<(), Self::Error> {
        Err(IrqControlError::Offline)
    }
}

fn admin_source(id: usize) -> DomainIrqSource {
    DomainIrqSource::new(
        IrqSourceId::new(id).unwrap(),
        BlockEvidenceSource::new(
            Box::new(TestEvidenceEndpoint),
            Box::new(TestEvidenceControl),
        ),
    )
}

fn logical_device(id: usize) -> LogicalDeviceCapability {
    LogicalDeviceCapability::new(
        driver_key(id),
        LogicalDeviceConstraints::discover_during_init(
            rdif_block::dma_api::DmaDomainId::legacy_global(),
            u64::MAX,
        ),
    )
}

fn domain(id: usize, device_id: usize, max_queues: u16) -> OwnershipDomainCapability {
    OwnershipDomainCapability::new(
        OwnershipDomainId::new(id).unwrap(),
        LogicalDeviceSelector::exact(vec![driver_key(device_id)]).unwrap(),
        QueueExecution::Tagged,
        NonZeroU16::new(1).unwrap(),
        NonZeroU16::new(max_queues).unwrap(),
        HardwareQueueDepth::new(NonZeroU16::new(1).unwrap(), NonZeroU16::new(128).unwrap())
            .unwrap(),
        rdif_block::IdList::from_bits(1 << id),
    )
    .unwrap()
}

fn optional_domain(id: usize, device_id: usize) -> OwnershipDomainCapability {
    OwnershipDomainCapability::new_optional(
        OwnershipDomainId::new(id).unwrap(),
        LogicalDeviceSelector::exact(vec![driver_key(device_id)]).unwrap(),
        QueueExecution::Tagged,
        NonZeroU16::MIN,
        NonZeroU16::MIN,
        HardwareQueueDepth::fixed(NonZeroU16::new(8).unwrap()),
        rdif_block::IdList::from_bits(1 << id),
    )
    .unwrap()
}

fn driver_key(id: usize) -> DriverDeviceKey {
    DriverDeviceKey::new(NonZeroU64::new(id as u64 + 1).unwrap())
}

#[test]
fn runtime_selects_queue_count_without_leaking_cpu_or_watchdog_policy() {
    let capabilities = ControllerCapabilities::new(
        NonZeroUsize::new(0x42).unwrap(),
        vec![logical_device(0)],
        vec![domain(0, 0, 8)],
    )
    .unwrap();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            OwnershipDomainId::new(0).unwrap(),
            NonZeroU16::new(4).unwrap(),
            NonZeroU16::new(8).unwrap(),
            rdif_block::IdList::from_bits(1),
        )],
    )
    .unwrap();

    assert_eq!(plan.controller_identity(), NonZeroUsize::new(0x42).unwrap());
    assert_eq!(plan.domains()[0].queue_count().get(), 4);

    let source = include_str!("../src/activation/mod.rs");
    assert!(!source.contains("watchdog"));
    assert!(!source.contains("workqueue"));
    assert!(!source.contains("cpu_id"));
    assert!(!source.contains("owner_cpu"));
}

#[test]
fn runtime_selects_hardware_queue_depth_from_the_domain_capability() {
    let depth = HardwareQueueDepth::new(NonZeroU16::new(2).unwrap(), NonZeroU16::new(128).unwrap())
        .unwrap();
    let capability = OwnershipDomainCapability::new(
        OwnershipDomainId::new(0).unwrap(),
        LogicalDeviceSelector::exact(vec![driver_key(0)]).unwrap(),
        QueueExecution::Tagged,
        NonZeroU16::new(1).unwrap(),
        NonZeroU16::new(8).unwrap(),
        depth,
        rdif_block::IdList::from_bits(1),
    )
    .unwrap();
    let capabilities = ControllerCapabilities::new(
        NonZeroUsize::new(0x43).unwrap(),
        vec![logical_device(0)],
        vec![capability],
    )
    .unwrap();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            OwnershipDomainId::new(0).unwrap(),
            NonZeroU16::new(4).unwrap(),
            NonZeroU16::new(64).unwrap(),
            rdif_block::IdList::from_bits(1),
        )],
    )
    .unwrap();

    assert_eq!(plan.domains()[0].queue_depth().get(), 64);
}

#[test]
fn plan_rejects_unknown_or_oversized_ownership_domains() {
    let capabilities = ControllerCapabilities::new(
        NonZeroUsize::new(7).unwrap(),
        vec![logical_device(0)],
        vec![domain(2, 0, 2)],
    )
    .unwrap();

    assert!(
        ActivationPlan::new(
            &capabilities,
            vec![DomainActivationPlan::new(
                OwnershipDomainId::new(1).unwrap(),
                NonZeroU16::new(1).unwrap(),
                NonZeroU16::new(8).unwrap(),
                rdif_block::IdList::from_bits(1 << 1),
            )],
        )
        .is_err()
    );
    assert!(
        ActivationPlan::new(
            &capabilities,
            vec![DomainActivationPlan::new(
                OwnershipDomainId::new(2).unwrap(),
                NonZeroU16::new(3).unwrap(),
                NonZeroU16::new(8).unwrap(),
                rdif_block::IdList::from_bits(1 << 2),
            )],
        )
        .is_err()
    );
}

#[test]
fn plan_may_omit_optional_domain_but_not_required_domain() {
    let capabilities = ControllerCapabilities::new(
        NonZeroUsize::new(8).unwrap(),
        vec![logical_device(0)],
        vec![domain(0, 0, 1), optional_domain(1, 0)],
    )
    .unwrap();
    let required = DomainActivationPlan::new(
        OwnershipDomainId::new(0).unwrap(),
        NonZeroU16::MIN,
        NonZeroU16::new(8).unwrap(),
        rdif_block::IdList::from_bits(1),
    );

    let plan = ActivationPlan::new(&capabilities, vec![required]).unwrap();
    assert!(plan.domain(OwnershipDomainId::new(1).unwrap()).is_none());

    let error = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            OwnershipDomainId::new(1).unwrap(),
            NonZeroU16::MIN,
            NonZeroU16::new(8).unwrap(),
            rdif_block::IdList::from_bits(1 << 1),
        )],
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActivationError::MissingDomainPlan {
            domain: OwnershipDomainId::new(0).unwrap(),
        }
    );
}

#[test]
fn capabilities_reject_duplicate_domain_ownership() {
    let error = ControllerCapabilities::new(
        NonZeroUsize::new(7).unwrap(),
        vec![logical_device(0)],
        vec![domain(0, 0, 1), domain(0, 0, 1)],
    )
    .unwrap_err();

    assert!(error.to_string().contains("duplicate ownership domain"));
}

#[test]
fn control_domain_mismatch_is_rejected_before_parts_can_be_published() {
    let identity = NonZeroUsize::new(7).unwrap();
    let io_domain = OwnershipDomainId::new(2).unwrap();
    let control_domain = OwnershipDomainId::new(6).unwrap();
    let capabilities = ControllerCapabilities::new_with_control_capability(
        identity,
        ControlDomainCapability::independent(
            control_domain,
            rdif_block::IdList::from_bits(1 << control_domain.get()),
        )
        .unwrap(),
        vec![logical_device(0)],
        vec![domain(io_domain.get(), 0, 1)],
    )
    .unwrap();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            io_domain,
            NonZeroU16::new(1).unwrap(),
            NonZeroU16::new(8).unwrap(),
            rdif_block::IdList::from_bits(1 << io_domain.get()),
        )],
    )
    .unwrap();
    let wrong_control = ControllerControlPart::new(
        OwnershipDomainId::new(5).unwrap(),
        Box::new(TestControl { identity }),
    );

    let error = PreparedControllerParts::new(plan, wrong_control).unwrap_err();

    assert_eq!(error.error(), &ActivationError::ControlDomainMismatch);
}

#[test]
fn independent_admin_control_domain_does_not_need_a_queue_or_device() {
    let control = ControlDomainCapability::independent(
        OwnershipDomainId::new(7).unwrap(),
        rdif_block::IdList::from_bits(1 << 7),
    )
    .unwrap();
    let capabilities = ControllerCapabilities::new_with_control_capability(
        NonZeroUsize::new(9).unwrap(),
        control,
        vec![logical_device(0)],
        vec![domain(2, 0, 2)],
    )
    .unwrap();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            OwnershipDomainId::new(2).unwrap(),
            NonZeroU16::new(1).unwrap(),
            NonZeroU16::new(8).unwrap(),
            rdif_block::IdList::from_bits(1 << 2),
        )],
    )
    .unwrap();

    assert_eq!(
        plan.control_activation(),
        ControlDomainActivation::Independent {
            domain: OwnershipDomainId::new(7).unwrap(),
            irq_sources: rdif_block::IdList::from_bits(1 << 7),
        }
    );
    assert!(plan.domain(OwnershipDomainId::new(7).unwrap()).is_none());
}

#[test]
fn independent_control_part_must_own_the_exact_selected_admin_irq_sources() {
    let identity = NonZeroUsize::new(13).unwrap();
    let capabilities = ControllerCapabilities::new_with_control_capability(
        identity,
        ControlDomainCapability::independent(
            OwnershipDomainId::new(7).unwrap(),
            rdif_block::IdList::from_bits(1 << 7),
        )
        .unwrap(),
        vec![logical_device(0)],
        vec![domain(2, 0, 1)],
    )
    .unwrap();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            OwnershipDomainId::new(2).unwrap(),
            NonZeroU16::new(1).unwrap(),
            NonZeroU16::new(8).unwrap(),
            rdif_block::IdList::from_bits(1 << 2),
        )],
    )
    .unwrap();
    let wrong_source = ControllerControlPart::new_independent(
        OwnershipDomainId::new(7).unwrap(),
        vec![admin_source(6)],
        Box::new(TestControl { identity }),
    )
    .unwrap();

    let error = PreparedControllerParts::new(plan, wrong_source).unwrap_err();

    assert_eq!(error.error(), &ActivationError::ControlIrqSourceMismatch);
}

#[test]
fn shared_control_domain_reuses_io_sources_without_duplicate_ownership() {
    let identity = NonZeroUsize::new(15).unwrap();
    let capabilities =
        ControllerCapabilities::new(identity, vec![logical_device(0)], vec![domain(2, 0, 1)])
            .unwrap();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            OwnershipDomainId::new(2).unwrap(),
            NonZeroU16::new(1).unwrap(),
            NonZeroU16::new(8).unwrap(),
            rdif_block::IdList::from_bits(1 << 2),
        )],
    )
    .unwrap();
    let part = ControllerControlPart::new_shared(
        OwnershipDomainId::new(2).unwrap(),
        vec![admin_source(2)],
        Box::new(TestControl { identity }),
    )
    .unwrap();

    assert!(matches!(
        plan.control_activation(),
        ControlDomainActivation::SharedWithIo { .. }
    ));
    assert_eq!(part.owned_irq_source_count(), 1);
}

#[test]
fn invalid_control_part_returns_every_consumed_owner() {
    let identity = NonZeroUsize::new(16).unwrap();
    let control_domain = OwnershipDomainId::new(7).unwrap();
    let duplicate_source = IrqSourceId::new(6).unwrap();
    let failure = ControllerControlPart::new_independent(
        control_domain,
        vec![admin_source(6), admin_source(6)],
        Box::new(TestControl { identity }),
    )
    .unwrap_err();

    assert_eq!(
        failure.error(),
        &ActivationError::DuplicateControlIrqSource {
            domain: control_domain,
            source_id: duplicate_source.get(),
        }
    );
    let (error, retained_domain, retained_sources, retained_control, combined_queues) =
        failure.into_parts();
    assert_eq!(
        error,
        ActivationError::DuplicateControlIrqSource {
            domain: control_domain,
            source_id: duplicate_source.get(),
        }
    );
    assert_eq!(retained_domain, control_domain);
    let ControlIrqOwnership::Independent(retained_sources) = retained_sources else {
        panic!("independent IRQ ownership must remain independent after rejection")
    };
    assert_eq!(retained_sources.len(), 2);
    assert_eq!(retained_control.controller_identity(), identity);
    assert!(combined_queues.is_none());
}

#[test]
fn portable_irq_source_identity_cannot_be_reused_by_two_io_domains() {
    let shared_source = rdif_block::IdList::from_bits(1 << 5);
    let make_domain = |domain_id, device_id| {
        OwnershipDomainCapability::new(
            OwnershipDomainId::new(domain_id).unwrap(),
            LogicalDeviceSelector::exact(vec![driver_key(device_id)]).unwrap(),
            QueueExecution::Tagged,
            NonZeroU16::new(1).unwrap(),
            NonZeroU16::new(1).unwrap(),
            HardwareQueueDepth::fixed(NonZeroU16::new(8).unwrap()),
            shared_source,
        )
        .unwrap()
    };

    let error = ControllerCapabilities::new_with_control_capability(
        NonZeroUsize::new(17).unwrap(),
        ControlDomainCapability::shared_with_io(OwnershipDomainId::new(1).unwrap(), shared_source)
            .unwrap(),
        vec![logical_device(0), logical_device(1)],
        vec![make_domain(1, 0), make_domain(2, 1)],
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ActivationError::OverlappingDomainIrqSource {
            source_id: 5,
            first_domain,
            second_domain,
        } if first_domain == OwnershipDomainId::new(1).unwrap()
            && second_domain == OwnershipDomainId::new(2).unwrap()
    ));
}
