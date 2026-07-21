use core::{
    num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize},
    pin::Pin,
};

use rdif_block::{
    ActivationError, ActivationPlan, BlkError, BlockEvidenceSource, ContainmentCause,
    ControlProgress, ControlSchedule, ControlTrigger, ControllerCapabilities, ControllerControl,
    ControllerControlPart, ControllerEpoch, ControllerPublicationFactory, ControllerReady,
    ControllerReinitialized, DomainActivationPlan, DomainIrqSource, DriverControlPoll,
    DriverControlTrigger, DriverDeviceKey, DriverGeneric, EvidenceClaim, EvidenceLatch,
    EvidenceServiceResult, HardwareQueueDepth, IrqCapture, IrqControlError, IrqEndpoint,
    IrqEventEpoch, IrqEvidenceId, IrqSourceControl, IrqSourceId, LifecycleEndpoint,
    LogicalDeviceCapability, LogicalDeviceConstraints, LogicalDeviceSelector, MaskedSource,
    OwnershipDomainCapability, OwnershipDomainId, PendingBlockIrq, PreparedControllerParts,
    QueueExecution,
};

#[test]
fn reinitialized_state_rejects_retained_irq_evidence() {
    let identity = NonZeroUsize::new(0x91).unwrap();
    let mut prepared = prepared_control(
        identity,
        InvalidControlResult::ReinitializedWithRetainedEvidence,
    );
    let latch = Box::pin(EvidenceLatch::new(IrqSourceId::new(2).unwrap()));
    let pending = pending_irq(latch.as_ref());

    let failure = prepared
        .service_control(ControlTrigger::Irq {
            now_ns: 200,
            evidence: pending,
        })
        .unwrap_err();

    assert_eq!(
        failure.error(),
        &ActivationError::ReinitializationWithUndrainedEvidence
    );
}

#[test]
fn control_schedule_rejects_irq_sources_outside_the_activation_plan() {
    let identity = NonZeroUsize::new(0x92).unwrap();
    let mut prepared = prepared_control(identity, InvalidControlResult::PendingForeignIrqSource);

    let failure = prepared
        .service_control(ControlTrigger::Start { now_ns: 100 })
        .unwrap_err();

    assert_eq!(
        failure.error(),
        &ActivationError::ControlScheduleIrqSourceMismatch {
            scheduled: rdif_block::IdList::from_bits(1 << 7),
            owned: rdif_block::IdList::from_bits(1 << 2),
        }
    );
}

#[derive(Clone, Copy)]
enum InvalidControlResult {
    ReinitializedWithRetainedEvidence,
    PendingForeignIrqSource,
}

struct InvalidControl {
    identity: NonZeroUsize,
    result: InvalidControlResult,
}

impl DriverGeneric for InvalidControl {
    fn name(&self) -> &str {
        "invalid-control"
    }
}

impl ControllerControl for InvalidControl {
    fn controller_identity(&self) -> NonZeroUsize {
        self.identity
    }

    fn service_control(
        &mut self,
        trigger: DriverControlTrigger,
        _publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        match (self.result, trigger) {
            (
                InvalidControlResult::ReinitializedWithRetainedEvidence,
                DriverControlTrigger::Irq { .. },
            ) => {
                // SAFETY: this deterministic fake has no DMA and keeps one
                // stable controller identity for the whole test.
                let ready =
                    unsafe { ControllerReady::new(ControllerEpoch::new(2), self.identity.get()) };
                let reinitialized =
                    ControllerReinitialized::new(ready, vec![OwnershipDomainId::new(2).unwrap()])
                        .unwrap();
                DriverControlPoll::after_irq(
                    ControlProgress::Reinitialized(reinitialized),
                    EvidenceServiceResult::Retained,
                )
            }
            (InvalidControlResult::PendingForeignIrqSource, DriverControlTrigger::Start { .. }) => {
                DriverControlPoll::without_evidence(ControlProgress::Pending(
                    ControlSchedule::new(false, rdif_block::IdList::from_bits(1 << 7), None)
                        .unwrap(),
                ))
            }
            _ => panic!("invalid-control received an unsupported test transition"),
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

fn prepared_control(
    identity: NonZeroUsize,
    result: InvalidControlResult,
) -> PreparedControllerParts {
    let domain = OwnershipDomainId::new(2).unwrap();
    let driver_key = DriverDeviceKey::new(NonZeroU64::new(1).unwrap());
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
                LogicalDeviceSelector::exact(vec![driver_key]).unwrap(),
                QueueExecution::Tagged,
                NonZeroU16::MIN,
                NonZeroU16::MIN,
                HardwareQueueDepth::fixed(NonZeroU16::new(8).unwrap()),
                rdif_block::IdList::from_bits(1 << 2),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            domain,
            NonZeroU16::MIN,
            NonZeroU16::new(8).unwrap(),
            rdif_block::IdList::from_bits(1 << 2),
        )],
    )
    .unwrap();
    let mut control = ControllerControlPart::new_shared(
        domain,
        vec![DomainIrqSource::new(
            IrqSourceId::new(2).unwrap(),
            BlockEvidenceSource::new(Box::new(EmptyEvidence), Box::new(EmptyIrqControl)),
        )],
        Box::new(InvalidControl { identity, result }),
    )
    .unwrap();
    for source in control.owned_irq_sources_mut() {
        let registration = source.take_for_registration().unwrap();
        source.finish_registration().unwrap();
        drop(registration);
    }
    PreparedControllerParts::new(plan, control).unwrap()
}

fn pending_irq(latch: Pin<&EvidenceLatch>) -> PendingBlockIrq {
    let evidence = IrqEvidenceId::new(
        IrqSourceId::new(2).unwrap(),
        NonZeroU64::new(1).unwrap(),
        0,
        NonZeroU32::new(1).unwrap(),
    );
    let EvidenceClaim::Claimed(claim) = latch.claim(evidence, None).unwrap() else {
        panic!("a fresh evidence latch must return its primary linear owner")
    };
    PendingBlockIrq::from_claim(claim, IrqEventEpoch::new(1).unwrap())
}
