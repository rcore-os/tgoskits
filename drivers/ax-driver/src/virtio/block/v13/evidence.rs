//! Linear ownership of destructively acknowledged VirtIO IRQ facts.

use alloc::{boxed::Box, sync::Arc};
use core::{
    hint::spin_loop,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
};

use rdif_block::{
    BlkError, BlockEvidenceSource, EvidenceServiceResult, FaultContainment, IrqCapture,
    IrqEndpoint, IrqEvidenceId, IrqSourceControl, IrqSourceId, MaskedSource,
    RecoveryEvidenceRetireFailure, RecoveryEvidenceRetirePermit, RecoveryEvidenceRetired,
};
use virtio_drivers::transport::InterruptStatus;

use super::super::irq::VirtioInterruptPort;

const VIRTIO_EVIDENCE_SLOT: u16 = 0;
const LEDGER_EMPTY: u8 = 0;
const LEDGER_PUBLISHING: u8 = 1;
const LEDGER_PUBLISHED: u8 = 2;
const LEDGER_SERVICING: u8 = 3;
const LEDGER_MERGING: u8 = 4;
const LEDGER_DRAIN_READY: u8 = 5;
const LEDGER_RETIRING: u8 = 6;

/// Driver-private facts stored behind one opaque IRQ evidence identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct VirtioBlockEvidenceFacts(u32);

impl VirtioBlockEvidenceFacts {
    pub(super) const NONE: Self = Self(0);
    pub(super) const QUEUE: Self = Self(InterruptStatus::QUEUE_INTERRUPT.bits());
    pub(super) const CONFIG: Self = Self(InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT.bits());

    const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    const fn is_empty(self) -> bool {
        self.0 == 0
    }

    const fn bits(self) -> u32 {
        self.0
    }

    pub(super) const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub(super) const fn without(self, consumed: Self) -> Self {
        Self(self.0 & !consumed.0)
    }

    pub(super) const fn retained_by_control(self, config_consumed: bool) -> Self {
        if config_consumed {
            self.without(Self::CONFIG)
        } else {
            self
        }
    }

    pub(super) const fn retained_by_io(self, queue_consumed: bool) -> Self {
        if queue_consumed {
            self.without(Self::QUEUE)
        } else {
            self
        }
    }

    pub(super) const fn unknown_bits(self) -> u32 {
        self.0 & !(Self::QUEUE.0 | Self::CONFIG.0)
    }
}

/// Fixed one-source ledger for destructively acknowledged VirtIO ISR facts.
pub(super) struct VirtioBlockEvidenceLedger {
    source: IrqSourceId,
    state: AtomicU8,
    lifecycle_generation: AtomicU64,
    slot_generation: AtomicU32,
    facts: AtomicU32,
}

impl VirtioBlockEvidenceLedger {
    pub(super) const fn new(source: IrqSourceId) -> Self {
        Self {
            source,
            state: AtomicU8::new(LEDGER_EMPTY),
            lifecycle_generation: AtomicU64::new(0),
            slot_generation: AtomicU32::new(0),
            facts: AtomicU32::new(0),
        }
    }

    fn publish(
        &self,
        lifecycle_generation: NonZeroU64,
        facts: VirtioBlockEvidenceFacts,
    ) -> Result<IrqEvidenceId, VirtioEvidenceError> {
        if facts.is_empty() {
            return Err(VirtioEvidenceError::EmptyFacts);
        }
        match self.state.load(Ordering::Acquire) {
            LEDGER_EMPTY => self.publish_fresh(lifecycle_generation, facts),
            LEDGER_PUBLISHED | LEDGER_SERVICING | LEDGER_DRAIN_READY => {
                self.merge_live(lifecycle_generation, facts)
            }
            LEDGER_PUBLISHING | LEDGER_MERGING => Err(VirtioEvidenceError::PublicationInProgress),
            _ => Err(VirtioEvidenceError::InvalidState),
        }
    }

    pub(super) fn begin_service(
        &self,
        evidence: IrqEvidenceId,
    ) -> Result<VirtioEvidenceBatch<'_>, VirtioEvidenceError> {
        self.validate_identity(evidence)?;
        self.state
            .compare_exchange(
                LEDGER_PUBLISHED,
                LEDGER_SERVICING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| VirtioEvidenceError::PublicationInProgress)?;
        let facts = VirtioBlockEvidenceFacts(self.facts.swap(0, Ordering::AcqRel));
        if facts.is_empty() {
            self.state.store(LEDGER_PUBLISHED, Ordering::Release);
            return Err(VirtioEvidenceError::InvalidState);
        }
        Ok(VirtioEvidenceBatch {
            ledger: self,
            evidence,
            facts,
            finished: false,
        })
    }

    pub(super) fn finish_service(
        &self,
        mut batch: VirtioEvidenceBatch<'_>,
        retained: VirtioBlockEvidenceFacts,
    ) -> EvidenceServiceResult {
        if !core::ptr::eq(self, batch.ledger) || self.validate_identity(batch.evidence).is_err() {
            return EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership);
        }
        self.facts.fetch_or(retained.bits(), Ordering::Release);
        let result = self.finish_active_service();
        batch.finished = true;
        result
    }

    pub(super) fn commit_drained_evidence(
        &self,
        evidence: IrqEvidenceId,
    ) -> Result<rdif_block::DriverEvidenceRetirement, VirtioEvidenceError> {
        self.validate_identity(evidence)?;
        loop {
            match self.state.load(Ordering::Acquire) {
                LEDGER_DRAIN_READY => {
                    let facts_arrived = self.facts.load(Ordering::Acquire) != 0;
                    let next = if facts_arrived {
                        LEDGER_PUBLISHED
                    } else {
                        LEDGER_EMPTY
                    };
                    if self
                        .state
                        .compare_exchange(
                            LEDGER_DRAIN_READY,
                            next,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Ok(if facts_arrived {
                            rdif_block::DriverEvidenceRetirement::Raced
                        } else {
                            rdif_block::DriverEvidenceRetirement::Retired
                        });
                    }
                }
                LEDGER_MERGING => spin_loop(),
                _ => return Err(VirtioEvidenceError::InvalidState),
            }
        }
    }

    pub(super) fn retire_after_quiesce(
        &self,
        permit: RecoveryEvidenceRetirePermit,
        owner: NonZeroUsize,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        permit.retire_with(owner, |evidence, _epoch| {
            self.validate_identity(evidence)
                .map_err(|_| BlkError::InvalidDmaProof)?;
            loop {
                let observed = self.state.load(Ordering::Acquire);
                match observed {
                    LEDGER_PUBLISHED | LEDGER_DRAIN_READY => {
                        if self
                            .state
                            .compare_exchange(
                                observed,
                                LEDGER_RETIRING,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_err()
                        {
                            continue;
                        }
                        self.facts.store(0, Ordering::Relaxed);
                        self.state.store(LEDGER_EMPTY, Ordering::Release);
                        return Ok(());
                    }
                    LEDGER_PUBLISHING | LEDGER_SERVICING | LEDGER_MERGING | LEDGER_RETIRING => {
                        return Err(BlkError::Busy);
                    }
                    LEDGER_EMPTY => {
                        return Err(BlkError::Other(
                            "VirtIO recovery evidence is no longer driver-owned",
                        ));
                    }
                    _ => return Err(BlkError::Quarantined),
                }
            }
        })
    }

    fn publish_fresh(
        &self,
        lifecycle_generation: NonZeroU64,
        facts: VirtioBlockEvidenceFacts,
    ) -> Result<IrqEvidenceId, VirtioEvidenceError> {
        self.state
            .compare_exchange(
                LEDGER_EMPTY,
                LEDGER_PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| VirtioEvidenceError::PublicationInProgress)?;
        let next_generation =
            if self.lifecycle_generation.load(Ordering::Relaxed) == lifecycle_generation.get() {
                self.slot_generation
                    .load(Ordering::Relaxed)
                    .checked_add(1)
                    .and_then(NonZeroU32::new)
            } else {
                NonZeroU32::new(1)
            };
        let Some(next_generation) = next_generation else {
            self.state.store(LEDGER_EMPTY, Ordering::Release);
            return Err(VirtioEvidenceError::GenerationExhausted);
        };
        self.lifecycle_generation
            .store(lifecycle_generation.get(), Ordering::Relaxed);
        self.slot_generation
            .store(next_generation.get(), Ordering::Relaxed);
        self.facts.store(facts.bits(), Ordering::Relaxed);
        self.state.store(LEDGER_PUBLISHED, Ordering::Release);
        Ok(IrqEvidenceId::new(
            self.source,
            lifecycle_generation,
            VIRTIO_EVIDENCE_SLOT,
            next_generation,
        ))
    }

    fn merge_live(
        &self,
        lifecycle_generation: NonZeroU64,
        facts: VirtioBlockEvidenceFacts,
    ) -> Result<IrqEvidenceId, VirtioEvidenceError> {
        if self.lifecycle_generation.load(Ordering::Acquire) != lifecycle_generation.get() {
            return Err(VirtioEvidenceError::LifecycleConflict);
        }
        let observed = self.state.load(Ordering::Acquire);
        if !matches!(
            observed,
            LEDGER_PUBLISHED | LEDGER_SERVICING | LEDGER_DRAIN_READY
        ) {
            return Err(VirtioEvidenceError::PublicationInProgress);
        }
        self.state
            .compare_exchange(
                observed,
                LEDGER_MERGING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| VirtioEvidenceError::PublicationInProgress)?;
        self.facts.fetch_or(facts.bits(), Ordering::Relaxed);
        let evidence = self.current_identity();
        // Preserve the exact transaction phase. In particular, a capture
        // racing runtime latch completion appends to DRAIN_READY instead of
        // minting a new identity; the later commit reports that race.
        self.state.store(observed, Ordering::Release);
        evidence
    }

    fn finish_active_service(&self) -> EvidenceServiceResult {
        loop {
            match self.state.load(Ordering::Acquire) {
                LEDGER_SERVICING => {
                    let retained = self.facts.load(Ordering::Acquire) != 0;
                    let next = if retained {
                        LEDGER_PUBLISHED
                    } else {
                        LEDGER_DRAIN_READY
                    };
                    if self
                        .state
                        .compare_exchange(
                            LEDGER_SERVICING,
                            next,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return if retained {
                            EvidenceServiceResult::Retained
                        } else {
                            EvidenceServiceResult::Drained
                        };
                    }
                }
                LEDGER_MERGING => spin_loop(),
                _ => {
                    return EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership);
                }
            }
        }
    }

    fn current_identity(&self) -> Result<IrqEvidenceId, VirtioEvidenceError> {
        let lifecycle_generation =
            NonZeroU64::new(self.lifecycle_generation.load(Ordering::Acquire))
                .ok_or(VirtioEvidenceError::InvalidState)?;
        let slot_generation = NonZeroU32::new(self.slot_generation.load(Ordering::Acquire))
            .ok_or(VirtioEvidenceError::InvalidState)?;
        Ok(IrqEvidenceId::new(
            self.source,
            lifecycle_generation,
            VIRTIO_EVIDENCE_SLOT,
            slot_generation,
        ))
    }

    fn validate_identity(&self, evidence: IrqEvidenceId) -> Result<(), VirtioEvidenceError> {
        if evidence.source() != self.source
            || evidence.slot() != VIRTIO_EVIDENCE_SLOT
            || evidence.device_generation().get()
                != self.lifecycle_generation.load(Ordering::Acquire)
            || evidence.slot_generation().get() != self.slot_generation.load(Ordering::Acquire)
        {
            return Err(VirtioEvidenceError::IdentityMismatch);
        }
        Ok(())
    }
}

pub(super) struct VirtioEvidenceBatch<'ledger> {
    ledger: &'ledger VirtioBlockEvidenceLedger,
    evidence: IrqEvidenceId,
    facts: VirtioBlockEvidenceFacts,
    finished: bool,
}

impl VirtioEvidenceBatch<'_> {
    pub(super) const fn facts(&self) -> VirtioBlockEvidenceFacts {
        self.facts
    }
}

impl Drop for VirtioEvidenceBatch<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.ledger
            .facts
            .fetch_or(self.facts.bits(), Ordering::Release);
        loop {
            match self.ledger.state.load(Ordering::Acquire) {
                LEDGER_SERVICING => {
                    if self
                        .ledger
                        .state
                        .compare_exchange(
                            LEDGER_SERVICING,
                            LEDGER_PUBLISHED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
                LEDGER_MERGING => spin_loop(),
                // A completed merger may already have restored PUBLISHED.
                LEDGER_PUBLISHED => return,
                _ => return,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum VirtioEvidenceError {
    #[error("VirtIO IRQ contained no hardware fact")]
    EmptyFacts,
    #[error("VirtIO IRQ evidence generation was exhausted")]
    GenerationExhausted,
    #[error("VirtIO IRQ evidence belongs to another lifecycle")]
    LifecycleConflict,
    #[error("VirtIO IRQ evidence identity is stale or foreign")]
    IdentityMismatch,
    #[error("VirtIO IRQ evidence publication is already changing")]
    PublicationInProgress,
    #[error("VirtIO IRQ evidence ledger is in an invalid state")]
    InvalidState,
}

pub(super) struct VirtioEvidenceIrqState {
    enabled: AtomicBool,
    lifecycle_generation: AtomicU64,
}

impl VirtioEvidenceIrqState {
    pub(super) const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            lifecycle_generation: AtomicU64::new(1),
        }
    }

    pub(super) fn enable(&self) {
        if self.enabled.swap(true, Ordering::AcqRel) {
            return;
        }
        if self
            .lifecycle_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            == 0
        {
            self.lifecycle_generation.store(1, Ordering::Release);
        }
    }

    fn generation(&self) -> NonZeroU64 {
        NonZeroU64::new(self.lifecycle_generation.load(Ordering::Acquire))
            .unwrap_or(NonZeroU64::MIN)
    }

    pub(super) fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
}

struct VirtioBlockEvidenceEndpoint {
    port: VirtioInterruptPort,
    ledger: Arc<VirtioBlockEvidenceLedger>,
    state: Arc<VirtioEvidenceIrqState>,
}

impl IrqEndpoint for VirtioBlockEvidenceEndpoint {
    type Event = IrqEvidenceId;
    type Fault = BlkError;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        if !self.state.enabled.load(Ordering::Acquire) {
            return IrqCapture::Unhandled;
        }
        let raw = self.port.capture_raw_status();
        if raw == 0 {
            return IrqCapture::Unhandled;
        }
        match self.ledger.publish(
            self.state.generation(),
            VirtioBlockEvidenceFacts::from_raw(raw),
        ) {
            Ok(evidence) => IrqCapture::Captured {
                event: evidence,
                masked: None,
            },
            Err(_) => IrqCapture::Fault {
                reason: BlkError::Other("VirtIO IRQ evidence ledger is unavailable"),
                containment: FaultContainment::Uncontained,
            },
        }
    }

    fn contain(
        &mut self,
        _cause: rdif_block::ContainmentCause,
    ) -> Result<MaskedSource, Self::Fault> {
        Err(BlkError::Other(
            "virtio interrupt source cannot be contained from hard IRQ",
        ))
    }
}

struct VirtioBlockEvidenceControl {
    state: Arc<VirtioEvidenceIrqState>,
}

impl IrqSourceControl for VirtioBlockEvidenceControl {
    type Error = rdif_block::IrqControlError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        let expected = self.state.generation().get();
        let actual = source.lifecycle_generation().get();
        if expected != actual {
            return Err(rdif_block::IrqControlError::StaleGeneration { expected, actual });
        }
        Err(rdif_block::IrqControlError::SourceNotMasked {
            bitmap: source.bitmap().get(),
        })
    }
}

pub(super) fn new_evidence_source(
    port: VirtioInterruptPort,
    ledger: Arc<VirtioBlockEvidenceLedger>,
    state: Arc<VirtioEvidenceIrqState>,
) -> BlockEvidenceSource {
    BlockEvidenceSource::new(
        Box::new(VirtioBlockEvidenceEndpoint {
            port,
            ledger,
            state: Arc::clone(&state),
        }),
        Box::new(VirtioBlockEvidenceControl { state }),
    )
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc};
    use core::{
        num::NonZeroUsize,
        sync::atomic::{AtomicU8, Ordering},
    };

    use rdif_block::{DriverEvidenceRetirement, EvidenceServiceResult, IrqCapture};
    use virtio_drivers::transport::InterruptStatus;

    use super::{
        LEDGER_DRAIN_READY, LEDGER_EMPTY, LEDGER_PUBLISHED, VirtioBlockEvidenceFacts,
        VirtioBlockEvidenceLedger, new_evidence_source,
    };
    use crate::virtio::block::{VIRTIO_BLK_IRQ_SOURCE_ID, irq::test_interrupt_port};

    fn test_source(
        status: Arc<AtomicU8>,
        ledger: Arc<VirtioBlockEvidenceLedger>,
    ) -> rdif_block::BlockEvidenceSource {
        let state = Arc::new(super::VirtioEvidenceIrqState::new());
        state.enable();
        new_evidence_source(test_interrupt_port(status), ledger, state)
    }

    #[test]
    fn shared_irq_without_virtio_status_is_unhandled() {
        let status = Arc::new(AtomicU8::new(0));
        let ledger = Arc::new(VirtioBlockEvidenceLedger::new(
            rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap(),
        ));
        let source = test_source(status, Arc::clone(&ledger));
        let (mut endpoint, _control) = source.into_parts();

        assert!(matches!(endpoint.capture(), IrqCapture::Unhandled));
        assert_eq!(ledger.state.load(Ordering::Acquire), LEDGER_EMPTY);
        assert_eq!(ledger.facts.load(Ordering::Acquire), 0);
    }

    #[test]
    fn config_only_interrupt_is_a_real_device_fact() {
        let status = Arc::new(AtomicU8::new(
            InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT.bits() as u8,
        ));
        let ledger = Arc::new(VirtioBlockEvidenceLedger::new(
            rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap(),
        ));
        let source = test_source(status, Arc::clone(&ledger));
        let (mut endpoint, _control) = source.into_parts();

        let IrqCapture::Captured {
            event,
            masked: None,
        } = endpoint.capture()
        else {
            panic!("config status is a device-owned hardware fact")
        };
        assert!(ledger.validate_identity(event).is_ok());
        assert_eq!(
            VirtioBlockEvidenceFacts(ledger.facts.load(Ordering::Acquire)),
            VirtioBlockEvidenceFacts::CONFIG
        );
    }

    #[test]
    fn evidence_capture_is_allocation_free_in_hard_irq() {
        let status = Arc::new(AtomicU8::new(InterruptStatus::QUEUE_INTERRUPT.bits() as u8));
        let ledger = Arc::new(VirtioBlockEvidenceLedger::new(
            rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap(),
        ));
        let source = test_source(status, ledger);
        let (mut endpoint, _control) = source.into_parts();

        let (capture, activity) = crate::test_klib::audit_allocations(|| endpoint.capture());

        assert!(matches!(capture, IrqCapture::Captured { masked: None, .. }));
        assert_eq!(
            activity,
            crate::test_klib::AllocationActivity {
                allocations: 0,
                deallocations: 0,
            }
        );
    }

    fn capture_combined(ledger: &Arc<VirtioBlockEvidenceLedger>) -> rdif_block::IrqEvidenceId {
        let raw = InterruptStatus::QUEUE_INTERRUPT.bits()
            | InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT.bits();
        let status = Arc::new(AtomicU8::new(raw as u8));
        let source = test_source(status, Arc::clone(ledger));
        let (mut endpoint, _control) = source.into_parts();
        let IrqCapture::Captured {
            event,
            masked: None,
        } = endpoint.capture()
        else {
            panic!("combined hardware facts must be captured")
        };
        event
    }

    enum EvidenceOwner {
        Control,
        Io,
    }

    fn consume_owner_fact(
        ledger: &VirtioBlockEvidenceLedger,
        evidence: rdif_block::IrqEvidenceId,
        owner: EvidenceOwner,
    ) -> EvidenceServiceResult {
        let batch = ledger.begin_service(evidence).unwrap();
        let facts = batch.facts();
        let retained = match owner {
            EvidenceOwner::Control => facts.retained_by_control(true),
            EvidenceOwner::Io => facts.retained_by_io(true),
        };
        ledger.finish_service(batch, retained)
    }

    #[test]
    fn combined_facts_are_linear_when_control_runs_before_io() {
        let ledger = Arc::new(VirtioBlockEvidenceLedger::new(
            rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap(),
        ));
        let evidence = capture_combined(&ledger);
        let mut configs = 0;
        let mut completions = 0;

        configs += 1;
        assert_eq!(
            consume_owner_fact(&ledger, evidence, EvidenceOwner::Control),
            EvidenceServiceResult::Retained
        );
        completions += 1;
        assert_eq!(
            consume_owner_fact(&ledger, evidence, EvidenceOwner::Io),
            EvidenceServiceResult::Drained
        );
        assert_eq!(
            ledger.commit_drained_evidence(evidence),
            Ok(DriverEvidenceRetirement::Retired)
        );
        assert_eq!((configs, completions), (1, 1));
    }

    #[test]
    fn combined_facts_are_linear_when_io_runs_before_control() {
        let ledger = Arc::new(VirtioBlockEvidenceLedger::new(
            rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap(),
        ));
        let evidence = capture_combined(&ledger);
        let mut configs = 0;
        let mut completions = 0;

        completions += 1;
        assert_eq!(
            consume_owner_fact(&ledger, evidence, EvidenceOwner::Io),
            EvidenceServiceResult::Retained
        );
        configs += 1;
        assert_eq!(
            consume_owner_fact(&ledger, evidence, EvidenceOwner::Control),
            EvidenceServiceResult::Drained
        );
        assert_eq!(
            ledger.commit_drained_evidence(evidence),
            Ok(DriverEvidenceRetirement::Retired)
        );
        assert_eq!((configs, completions), (1, 1));
    }

    #[test]
    fn merged_capture_cannot_start_a_second_service_while_first_batch_is_live() {
        let ledger = VirtioBlockEvidenceLedger::new(
            rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap(),
        );
        let lifecycle = core::num::NonZeroU64::MIN;
        let evidence = ledger
            .publish(lifecycle, VirtioBlockEvidenceFacts::QUEUE)
            .unwrap();
        let first = ledger.begin_service(evidence).unwrap();

        assert_eq!(
            ledger.publish(lifecycle, VirtioBlockEvidenceFacts::CONFIG),
            Ok(evidence)
        );
        assert!(matches!(
            ledger.begin_service(evidence),
            Err(super::VirtioEvidenceError::PublicationInProgress)
        ));
        assert_eq!(
            ledger.finish_service(first, VirtioBlockEvidenceFacts::NONE),
            EvidenceServiceResult::Retained
        );
        let second = ledger.begin_service(evidence).unwrap();
        assert_eq!(second.facts(), VirtioBlockEvidenceFacts::CONFIG);
        assert_eq!(
            ledger.finish_service(second, VirtioBlockEvidenceFacts::NONE),
            EvidenceServiceResult::Drained
        );
        assert_eq!(
            ledger.commit_drained_evidence(evidence),
            Ok(DriverEvidenceRetirement::Retired)
        );
    }

    #[test]
    fn drained_evidence_keeps_its_identity_until_runtime_commit() {
        let ledger = VirtioBlockEvidenceLedger::new(
            rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap(),
        );
        let lifecycle = core::num::NonZeroU64::MIN;
        let evidence = ledger
            .publish(lifecycle, VirtioBlockEvidenceFacts::QUEUE)
            .unwrap();
        let batch = ledger.begin_service(evidence).unwrap();

        assert_eq!(
            ledger.finish_service(batch, VirtioBlockEvidenceFacts::NONE),
            EvidenceServiceResult::Drained
        );
        assert_eq!(ledger.state.load(Ordering::Acquire), LEDGER_DRAIN_READY);

        assert_eq!(
            ledger.publish(lifecycle, VirtioBlockEvidenceFacts::CONFIG),
            Ok(evidence)
        );
        assert_eq!(
            ledger.commit_drained_evidence(evidence),
            Ok(DriverEvidenceRetirement::Raced)
        );
        assert_eq!(ledger.state.load(Ordering::Acquire), LEDGER_PUBLISHED);

        let raced_batch = ledger.begin_service(evidence).unwrap();
        assert_eq!(raced_batch.facts(), VirtioBlockEvidenceFacts::CONFIG);
        assert_eq!(
            ledger.finish_service(raced_batch, VirtioBlockEvidenceFacts::NONE),
            EvidenceServiceResult::Drained
        );
        assert_eq!(
            ledger.commit_drained_evidence(evidence),
            Ok(DriverEvidenceRetirement::Retired)
        );
        assert_eq!(ledger.state.load(Ordering::Acquire), LEDGER_EMPTY);
    }

    #[test]
    fn irq_capture_racing_runtime_commit_reuses_the_drained_identity() {
        let status = Arc::new(AtomicU8::new(InterruptStatus::QUEUE_INTERRUPT.bits() as u8));
        let ledger = Arc::new(VirtioBlockEvidenceLedger::new(
            rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap(),
        ));
        let source = test_source(Arc::clone(&status), Arc::clone(&ledger));
        let (mut endpoint, _control) = source.into_parts();
        let IrqCapture::Captured {
            event: evidence,
            masked: None,
        } = endpoint.capture()
        else {
            panic!("queue IRQ must publish its first evidence identity")
        };
        let batch = ledger.begin_service(evidence).unwrap();
        assert_eq!(
            ledger.finish_service(batch, VirtioBlockEvidenceFacts::NONE),
            EvidenceServiceResult::Drained
        );

        status.store(
            InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT.bits() as u8,
            Ordering::Release,
        );
        assert!(matches!(
            endpoint.capture(),
            IrqCapture::Captured {
                event,
                masked: None,
            } if event == evidence
        ));
        assert_eq!(
            ledger.commit_drained_evidence(evidence),
            Ok(DriverEvidenceRetirement::Raced)
        );
    }

    #[test]
    fn clean_commit_retires_identity_before_the_next_capture() {
        let ledger = VirtioBlockEvidenceLedger::new(
            rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap(),
        );
        let lifecycle = core::num::NonZeroU64::MIN;
        let first = ledger
            .publish(lifecycle, VirtioBlockEvidenceFacts::QUEUE)
            .unwrap();
        let batch = ledger.begin_service(first).unwrap();

        assert_eq!(
            ledger.finish_service(batch, VirtioBlockEvidenceFacts::NONE),
            EvidenceServiceResult::Drained
        );
        assert!(matches!(
            ledger.begin_service(first),
            Err(super::VirtioEvidenceError::PublicationInProgress)
        ));
        assert_eq!(
            ledger.commit_drained_evidence(first),
            Ok(DriverEvidenceRetirement::Retired)
        );

        let next = ledger
            .publish(lifecycle, VirtioBlockEvidenceFacts::CONFIG)
            .unwrap();
        assert_ne!(next.slot_generation(), first.slot_generation());
    }

    #[test]
    fn recovery_retire_clears_published_facts_but_rejects_a_live_service_batch() {
        let source = rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap();
        let ledger = VirtioBlockEvidenceLedger::new(source);
        let lifecycle = core::num::NonZeroU64::new(7).unwrap();
        let evidence = ledger
            .publish(lifecycle, VirtioBlockEvidenceFacts::QUEUE)
            .unwrap();
        let owner = NonZeroUsize::new(0x7171).unwrap();
        let batch = ledger.begin_service(evidence).unwrap();

        let failure = ledger
            .retire_after_quiesce(retirement_permit(evidence, owner), owner)
            .expect_err("SERVICING evidence must retain its exclusive batch owner");
        assert_eq!(
            ledger.state.load(Ordering::Acquire),
            super::LEDGER_SERVICING
        );
        let (_, permit) = failure.into_parts();
        drop(batch);
        assert_eq!(ledger.state.load(Ordering::Acquire), LEDGER_PUBLISHED);

        let retired = ledger.retire_after_quiesce(permit, owner).unwrap();
        assert_eq!(retired.evidence_id(), evidence);
        assert_eq!(ledger.state.load(Ordering::Acquire), LEDGER_EMPTY);
        assert_eq!(ledger.facts.load(Ordering::Acquire), 0);
        assert!(
            ledger
                .publish(
                    core::num::NonZeroU64::new(8).unwrap(),
                    VirtioBlockEvidenceFacts::CONFIG,
                )
                .is_ok()
        );
    }

    #[test]
    fn recovery_retire_clears_drain_ready_identity() {
        let source = rdif_block::IrqSourceId::new(VIRTIO_BLK_IRQ_SOURCE_ID).unwrap();
        let ledger = VirtioBlockEvidenceLedger::new(source);
        let evidence = ledger
            .publish(
                core::num::NonZeroU64::new(9).unwrap(),
                VirtioBlockEvidenceFacts::QUEUE,
            )
            .unwrap();
        let batch = ledger.begin_service(evidence).unwrap();
        assert_eq!(
            ledger.finish_service(batch, VirtioBlockEvidenceFacts::NONE),
            EvidenceServiceResult::Drained
        );
        let owner = NonZeroUsize::new(0x8181).unwrap();
        let _receipt = ledger
            .retire_after_quiesce(retirement_permit(evidence, owner), owner)
            .unwrap();
        assert_eq!(ledger.state.load(Ordering::Acquire), LEDGER_EMPTY);
    }

    fn retirement_permit(
        evidence: rdif_block::IrqEvidenceId,
        owner: NonZeroUsize,
    ) -> rdif_block::RecoveryEvidenceRetirePermit {
        let latch = Box::pin(rdif_block::EvidenceLatch::new(evidence.source()));
        let rdif_block::EvidenceClaim::Claimed(claim) =
            latch.as_ref().claim(evidence, None).unwrap()
        else {
            unreachable!()
        };
        let pending = rdif_block::PendingBlockIrq::from_claim(
            claim,
            rdif_block::IrqEventEpoch::new(1).unwrap(),
        );
        // SAFETY: this ledger-only test models synchronized IRQ and DMA state
        // for one retained controller owner.
        let proof = unsafe {
            rdif_block::DmaQuiesced::new(rdif_block::ControllerEpoch::new(2), owner.get())
        };
        let quiesced = pending.retire_after_quiesce(&proof, owner.get()).unwrap();
        let rdif_block::QuiescedEvidenceCompletion::Complete { permit } =
            quiesced.complete(latch.as_ref()).unwrap()
        else {
            unreachable!()
        };
        permit
    }
}
