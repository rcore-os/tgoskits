//! Linear ownership of acknowledged block-device interrupt evidence.

use core::{
    fmt,
    marker::PhantomPinned,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    pin::Pin,
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
};

use crate::{
    BlkError, ControllerEpoch, DmaQuiesced, IrqEventEpoch, IrqSourceControl, MaskedSource,
};

/// Maximum number of controller-local IRQ source identities in one activation.
pub const MAX_CONTROLLER_IRQ_SOURCES: usize = u64::BITS as usize;

/// Checked controller-local identity of one interrupt source.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IrqSourceId(u8);

impl IrqSourceId {
    pub const fn new(value: usize) -> Result<Self, EvidenceError> {
        if value < MAX_CONTROLLER_IRQ_SOURCES {
            Ok(Self(value as u8))
        } else {
            Err(EvidenceError::InvalidSourceId { value })
        }
    }

    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// Stable identity of one acknowledged entry in a driver-owned evidence ledger.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IrqEvidenceId {
    device_generation: NonZeroU64,
    slot_generation: NonZeroU32,
    slot: u16,
    source: IrqSourceId,
}

impl IrqEvidenceId {
    pub const fn new(
        source: IrqSourceId,
        device_generation: NonZeroU64,
        slot: u16,
        slot_generation: NonZeroU32,
    ) -> Self {
        Self {
            device_generation,
            slot_generation,
            slot,
            source,
        }
    }

    pub const fn source(self) -> IrqSourceId {
        self.source
    }

    pub const fn device_generation(self) -> NonZeroU64 {
        self.device_generation
    }

    pub const fn slot(self) -> u16 {
        self.slot
    }

    pub const fn slot_generation(self) -> NonZeroU32 {
        self.slot_generation
    }
}

const LATCH_IDLE: u8 = 0;
const LATCH_WRITING: u8 = 1;
const LATCH_ACTIVE: u8 = 2;
const LATCH_DIRTY: u8 = 3;
const LATCH_COMPLETING: u8 = 4;
const LATCH_FAULTED: u8 = 5;
const LATCH_COALESCING: u8 = 6;
const LATCH_COMPLETING_COALESCING: u8 = 7;
const LATCH_COMPLETING_DIRTY: u8 = 8;

/// Fixed, allocation-free single-owner latch for one IRQ source.
///
/// `claim` must be called on the pinned source object. The first captured
/// evidence mints a move-only token. A duplicate of the same evidence marks
/// the latch dirty, so drain performs a clear-and-recheck pass instead of
/// rearming. A different outstanding identity faults the source.
///
/// The latch is address-sensitive because claim tokens bind to its address.
/// Safe callers must pin it before the first capture:
///
/// ```compile_fail
/// use core::pin::Pin;
/// use rdif_block::{EvidenceLatch, IrqSourceId};
///
/// let latch = EvidenceLatch::new(IrqSourceId::new(0).unwrap());
/// let _ = Pin::new(&latch);
/// ```
pub struct EvidenceLatch {
    source: IrqSourceId,
    state: AtomicU8,
    sequence: AtomicU64,
    evidence_generation: AtomicU64,
    evidence_slot: AtomicU64,
    mask_lifecycle_generation: AtomicU64,
    mask_epoch: AtomicU64,
    mask_bitmap: AtomicU64,
    _pin: PhantomPinned,
}

impl EvidenceLatch {
    /// Creates one address-sensitive latch for an exact IRQ source.
    pub const fn new(source: IrqSourceId) -> Self {
        Self {
            source,
            state: AtomicU8::new(LATCH_IDLE),
            sequence: AtomicU64::new(0),
            evidence_generation: AtomicU64::new(0),
            evidence_slot: AtomicU64::new(0),
            mask_lifecycle_generation: AtomicU64::new(0),
            mask_epoch: AtomicU64::new(0),
            mask_bitmap: AtomicU64::new(0),
            _pin: PhantomPinned,
        }
    }

    /// Claims or coalesces a driver-ledger identity from hard IRQ context.
    pub fn claim(
        self: Pin<&Self>,
        evidence: IrqEvidenceId,
        masked: Option<MaskedSource>,
    ) -> Result<EvidenceClaim, EvidenceLatchError> {
        let this = self.get_ref();
        if evidence.source() != this.source {
            this.state.store(LATCH_FAULTED, Ordering::Release);
            return Err(EvidenceLatchError::WrongSource {
                configured: this.source,
                captured: evidence.source(),
            });
        }
        for _ in 0..16 {
            match this.state.load(Ordering::Acquire) {
                LATCH_IDLE => {
                    if this
                        .state
                        .compare_exchange(
                            LATCH_IDLE,
                            LATCH_WRITING,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        )
                        .is_err()
                    {
                        continue;
                    }
                    let sequence = this
                        .sequence
                        .fetch_add(1, Ordering::Relaxed)
                        .wrapping_add(1);
                    if sequence == 0 {
                        this.state.store(LATCH_FAULTED, Ordering::Release);
                        return Err(EvidenceLatchError::SequenceExhausted);
                    }
                    this.evidence_generation
                        .store(evidence.device_generation.get(), Ordering::Relaxed);
                    this.evidence_slot.store(
                        u64::from(evidence.slot_generation.get())
                            | (u64::from(evidence.slot) << 32)
                            | ((evidence.source.get() as u64) << 48),
                        Ordering::Relaxed,
                    );
                    if let Err(error) = this.initialize_mask(evidence, masked) {
                        this.state.store(LATCH_FAULTED, Ordering::Release);
                        return Err(error);
                    }
                    this.state.store(LATCH_ACTIVE, Ordering::Release);
                    return Ok(EvidenceClaim::Claimed(EvidenceClaimToken {
                        evidence,
                        latch_cookie: this as *const Self as usize,
                        sequence,
                    }));
                }
                state @ (LATCH_ACTIVE | LATCH_DIRTY) => {
                    if this
                        .state
                        .compare_exchange(
                            state,
                            LATCH_COALESCING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        continue;
                    }
                    return this.coalesce_or_fault(evidence, masked, LATCH_DIRTY);
                }
                state @ (LATCH_COMPLETING | LATCH_COMPLETING_DIRTY) => {
                    if this
                        .state
                        .compare_exchange(
                            state,
                            LATCH_COMPLETING_COALESCING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        continue;
                    }
                    return this.coalesce_or_fault(evidence, masked, LATCH_COMPLETING_DIRTY);
                }
                LATCH_WRITING | LATCH_COALESCING | LATCH_COMPLETING_COALESCING => {
                    return Err(EvidenceLatchError::TransitionContended);
                }
                LATCH_FAULTED => return Err(EvidenceLatchError::Faulted),
                _ => return Err(EvidenceLatchError::InvalidState),
            }
        }
        Err(EvidenceLatchError::TransitionContended)
    }

    fn coalesce_or_fault(
        &self,
        evidence: IrqEvidenceId,
        masked: Option<MaskedSource>,
        completed_state: u8,
    ) -> Result<EvidenceClaim, EvidenceLatchError> {
        let active = match self.read_evidence() {
            Ok(active) => active,
            Err(error) => {
                self.state.store(LATCH_FAULTED, Ordering::Release);
                return Err(error);
            }
        };
        if active != evidence {
            self.state.store(LATCH_FAULTED, Ordering::Release);
            return Err(EvidenceLatchError::ConflictingEvidence {
                active,
                captured: evidence,
            });
        }
        if let Err(error) = self.merge_mask(evidence, masked) {
            self.state.store(LATCH_FAULTED, Ordering::Release);
            return Err(error);
        }
        self.state.store(completed_state, Ordering::Release);
        Ok(EvidenceClaim::Coalesced)
    }

    fn initialize_mask(
        &self,
        evidence: IrqEvidenceId,
        masked: Option<MaskedSource>,
    ) -> Result<(), EvidenceLatchError> {
        self.mask_lifecycle_generation.store(0, Ordering::Relaxed);
        self.mask_epoch.store(0, Ordering::Relaxed);
        self.mask_bitmap.store(0, Ordering::Relaxed);
        self.merge_mask(evidence, masked)
    }

    fn merge_mask(
        &self,
        evidence: IrqEvidenceId,
        masked: Option<MaskedSource>,
    ) -> Result<(), EvidenceLatchError> {
        let Some(masked) = masked else {
            return Ok(());
        };
        if masked.lifecycle_generation() != evidence.device_generation() {
            return Err(EvidenceLatchError::MaskLifecycleGenerationMismatch {
                evidence: evidence.device_generation().get(),
                masked: masked.lifecycle_generation().get(),
            });
        }

        let active_bitmap = self.mask_bitmap.load(Ordering::Relaxed);
        if active_bitmap == 0 {
            self.mask_lifecycle_generation
                .store(masked.lifecycle_generation().get(), Ordering::Relaxed);
            self.mask_epoch
                .store(masked.mask_epoch().get(), Ordering::Relaxed);
            self.mask_bitmap
                .store(masked.bitmap().get(), Ordering::Relaxed);
            return Ok(());
        }

        let active_lifecycle =
            NonZeroU64::new(self.mask_lifecycle_generation.load(Ordering::Relaxed))
                .ok_or(EvidenceLatchError::InvalidState)?;
        let active_epoch = NonZeroU64::new(self.mask_epoch.load(Ordering::Relaxed))
            .ok_or(EvidenceLatchError::InvalidState)?;
        let active_bitmap =
            NonZeroU64::new(active_bitmap).ok_or(EvidenceLatchError::InvalidState)?;
        let active = MaskedSource::new_with_epoch(active_lifecycle, active_epoch, active_bitmap);
        let merged = active.try_union(masked).map_err(|error| {
            let (active, captured) = match error {
                rdif_irq::MaskedSourceUnionError::LifecycleGenerationMismatch {
                    active,
                    captured,
                }
                | rdif_irq::MaskedSourceUnionError::MaskEpochMismatch { active, captured } => {
                    (active, captured)
                }
            };
            EvidenceLatchError::ConflictingMaskIdentity {
                active_lifecycle: active.lifecycle_generation().get(),
                active_epoch: active.mask_epoch().get(),
                captured_lifecycle: captured.lifecycle_generation().get(),
                captured_epoch: captured.mask_epoch().get(),
            }
        })?;
        self.mask_bitmap
            .store(merged.bitmap().get(), Ordering::Relaxed);
        Ok(())
    }

    fn read_evidence(&self) -> Result<IrqEvidenceId, EvidenceLatchError> {
        let generation = self.evidence_generation.load(Ordering::Acquire);
        let slot = self.evidence_slot.load(Ordering::Acquire);
        let Some(device_generation) = NonZeroU64::new(generation) else {
            return Err(EvidenceLatchError::InvalidState);
        };
        let Some(slot_generation) = NonZeroU32::new(slot as u32) else {
            return Err(EvidenceLatchError::InvalidState);
        };
        let source = IrqSourceId::new(((slot >> 48) & 0xff) as usize)
            .map_err(|_| EvidenceLatchError::InvalidState)?;
        Ok(IrqEvidenceId::new(
            source,
            device_generation,
            ((slot >> 32) & 0xffff) as u16,
            slot_generation,
        ))
    }

    fn read_mask(&self) -> Result<Option<MaskedSource>, EvidenceLatchError> {
        let lifecycle_generation = self.mask_lifecycle_generation.load(Ordering::Acquire);
        let mask_epoch = self.mask_epoch.load(Ordering::Acquire);
        let bitmap = self.mask_bitmap.load(Ordering::Acquire);
        if bitmap == 0 {
            return if lifecycle_generation == 0 && mask_epoch == 0 {
                Ok(None)
            } else {
                Err(EvidenceLatchError::InvalidState)
            };
        }
        let Some(lifecycle_generation) = NonZeroU64::new(lifecycle_generation) else {
            return Err(EvidenceLatchError::InvalidState);
        };
        let Some(mask_epoch) = NonZeroU64::new(mask_epoch) else {
            return Err(EvidenceLatchError::InvalidState);
        };
        let Some(bitmap) = NonZeroU64::new(bitmap) else {
            return Err(EvidenceLatchError::InvalidState);
        };
        Ok(Some(MaskedSource::new_with_epoch(
            lifecycle_generation,
            mask_epoch,
            bitmap,
        )))
    }

    fn complete_claim(
        self: Pin<&Self>,
        token: &EvidenceClaimToken,
    ) -> Result<LatchCompletion, EvidenceLatchError> {
        let this = self.get_ref();
        if token.latch_cookie != this as *const Self as usize {
            return Err(EvidenceLatchError::ForeignClaim);
        }
        if token.sequence != this.sequence.load(Ordering::Acquire) {
            return Err(EvidenceLatchError::StaleClaim);
        }
        for _ in 0..16 {
            match this.state.load(Ordering::Acquire) {
                LATCH_ACTIVE => {
                    if this
                        .state
                        .compare_exchange(
                            LATCH_ACTIVE,
                            LATCH_COMPLETING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        continue;
                    }
                }
                LATCH_COMPLETING => {
                    let masked = match this.read_mask() {
                        Ok(masked) => masked,
                        Err(error) => {
                            this.state.store(LATCH_FAULTED, Ordering::Release);
                            return Err(error);
                        }
                    };
                    match this.state.compare_exchange(
                        LATCH_COMPLETING,
                        LATCH_IDLE,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return Ok(LatchCompletion::Clean(masked)),
                        Err(LATCH_COMPLETING_COALESCING | LATCH_COMPLETING_DIRTY) => continue,
                        Err(LATCH_FAULTED) => return Err(EvidenceLatchError::Faulted),
                        Err(_) => return Err(EvidenceLatchError::InvalidState),
                    }
                }
                state @ (LATCH_DIRTY | LATCH_COMPLETING_DIRTY) => {
                    if this
                        .state
                        .compare_exchange(state, LATCH_ACTIVE, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                    {
                        continue;
                    }
                    return Ok(LatchCompletion::Redeliver(EvidenceClaimToken {
                        evidence: token.evidence,
                        latch_cookie: token.latch_cookie,
                        sequence: token.sequence,
                    }));
                }
                LATCH_COALESCING | LATCH_COMPLETING_COALESCING => core::hint::spin_loop(),
                LATCH_FAULTED => return Err(EvidenceLatchError::Faulted),
                _ => return Err(EvidenceLatchError::InvalidState),
            }
        }
        Err(EvidenceLatchError::TransitionContended)
    }
}

/// Outcome of one hard-IRQ latch claim.
#[derive(Debug, Eq, PartialEq)]
pub enum EvidenceClaim {
    Claimed(EvidenceClaimToken),
    Coalesced,
}

/// Move-only proof minted by one exact pinned [`EvidenceLatch`].
#[derive(Debug, Eq, PartialEq)]
pub struct EvidenceClaimToken {
    evidence: IrqEvidenceId,
    latch_cookie: usize,
    sequence: u64,
}

enum LatchCompletion {
    Clean(Option<MaskedSource>),
    Redeliver(EvidenceClaimToken),
}

/// Move-only runtime owner of one captured device event.
#[derive(Debug, Eq, PartialEq)]
pub struct PendingBlockIrq {
    claim: EvidenceClaimToken,
    source_epoch: IrqEventEpoch,
}

impl PendingBlockIrq {
    /// Joins a unique source-latch claim with the OS source epoch.
    pub const fn from_claim(claim: EvidenceClaimToken, source_epoch: IrqEventEpoch) -> Self {
        Self {
            claim,
            source_epoch,
        }
    }

    pub const fn evidence_id(&self) -> IrqEvidenceId {
        self.claim.evidence
    }

    pub const fn source_epoch(&self) -> IrqEventEpoch {
        self.source_epoch
    }

    pub fn retain(self) -> IrqServiceDecision {
        IrqServiceDecision::Retained(self)
    }

    pub fn drain(self) -> IrqServiceDecision {
        IrqServiceDecision::Drained(DrainedEvidence(self))
    }

    pub fn recover(self, fault: ControllerFault) -> IrqServiceDecision {
        IrqServiceDecision::Recover {
            evidence: self,
            fault,
        }
    }

    /// Converts recovery-bound evidence into a source-retirement owner.
    ///
    /// This path is distinct from [`Self::drain`]: the controller has stopped
    /// every matching DMA engine and IRQ action, so the old latch must be
    /// cleared without producing a permission to rearm its device source.
    ///
    /// # Errors
    ///
    /// Returns the unchanged evidence owner when `proof` belongs to another
    /// controller instance.
    pub fn retire_after_quiesce(
        self,
        proof: &DmaQuiesced,
        controller_cookie: usize,
    ) -> Result<QuiescedEvidence, EvidenceRetireFailure> {
        let Some(controller_identity) = NonZeroUsize::new(controller_cookie) else {
            return Err(EvidenceRetireFailure {
                error: EvidenceRetireError::InvalidControllerIdentity,
                pending: self,
            });
        };
        if proof.controller_cookie() != controller_cookie {
            return Err(EvidenceRetireFailure {
                error: EvidenceRetireError::ForeignController,
                pending: self,
            });
        }
        Ok(QuiescedEvidence {
            pending: self,
            controller_identity,
            quiesce_epoch: proof.epoch(),
        })
    }
}

/// Controller-proof validation failure while retiring recovery evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EvidenceRetireError {
    /// A stable driver owner identity is required for terminal ledger retire.
    #[error("controller identity is zero")]
    InvalidControllerIdentity,
    /// The DMA proof belongs to another retained controller instance.
    #[error("DMA-quiescence proof belongs to another controller")]
    ForeignController,
}

/// Failed quiesced retirement retaining the exact evidence owner.
#[derive(Debug)]
#[must_use = "retry with the matching controller proof or retain evidence in quarantine"]
pub struct EvidenceRetireFailure {
    error: EvidenceRetireError,
    pending: PendingBlockIrq,
}

impl EvidenceRetireFailure {
    /// Returns the validation failure without consuming retained evidence.
    pub const fn error(&self) -> EvidenceRetireError {
        self.error
    }

    /// Recovers the error and unchanged move-only evidence owner.
    pub fn into_parts(self) -> (EvidenceRetireError, PendingBlockIrq) {
        (self.error, self.pending)
    }
}

impl fmt::Display for EvidenceRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl core::error::Error for EvidenceRetireFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Move-only owner of recovery evidence after matching DMA quiescence.
#[derive(Debug, Eq, PartialEq)]
pub struct QuiescedEvidence {
    pending: PendingBlockIrq,
    controller_identity: NonZeroUsize,
    quiesce_epoch: ControllerEpoch,
}

impl QuiescedEvidence {
    /// Clears the source latch without authorizing device-source rearm.
    ///
    /// A dirty latch returns another owner for the same evidence. The caller
    /// must repeat the bounded clear after the IRQ action has been disabled
    /// and synchronized.
    pub fn complete(
        self,
        latch: Pin<&EvidenceLatch>,
    ) -> Result<QuiescedEvidenceCompletion, (Self, EvidenceLatchError)> {
        let completion = match latch.complete_claim(&self.pending.claim) {
            Ok(completion) => completion,
            Err(error) => return Err((self, error)),
        };
        let Self {
            pending:
                PendingBlockIrq {
                    claim,
                    source_epoch,
                },
            controller_identity,
            quiesce_epoch,
        } = self;
        match completion {
            LatchCompletion::Clean(_masked) => Ok(QuiescedEvidenceCompletion::Complete {
                permit: RecoveryEvidenceRetirePermit {
                    controller_identity,
                    evidence: claim.evidence,
                    quiesce_epoch,
                },
            }),
            LatchCompletion::Redeliver(claim) => Ok(QuiescedEvidenceCompletion::Redeliver(Self {
                pending: PendingBlockIrq {
                    claim,
                    source_epoch,
                },
                controller_identity,
                quiesce_epoch,
            })),
        }
    }
}

/// Move-only permission to retire one exact driver-ledger identity.
///
/// This capability is minted only after the OS source latch completed under a
/// matching controller DMA-quiescence proof. Portable drivers must consume it
/// only after atomically removing the same evidence identity from their private
/// ledger. A failed driver transition returns the unchanged capability.
#[derive(Debug, Eq, PartialEq)]
#[must_use = "retire the matching driver ledger or retain this capability in quarantine"]
pub struct RecoveryEvidenceRetirePermit {
    controller_identity: NonZeroUsize,
    evidence: IrqEvidenceId,
    quiesce_epoch: ControllerEpoch,
}

impl RecoveryEvidenceRetirePermit {
    pub const fn controller_identity(&self) -> usize {
        self.controller_identity.get()
    }

    pub const fn source(&self) -> IrqSourceId {
        self.evidence.source()
    }

    pub const fn evidence_id(&self) -> IrqEvidenceId {
        self.evidence
    }

    pub const fn quiesce_epoch(&self) -> ControllerEpoch {
        self.quiesce_epoch
    }

    /// Executes one exact driver-ledger retirement and mints its terminal
    /// receipt only after the transition succeeds.
    pub fn retire_with(
        self,
        owner: NonZeroUsize,
        retire: impl FnOnce(IrqEvidenceId, ControllerEpoch) -> Result<(), BlkError>,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        if owner != self.controller_identity {
            return Err(RecoveryEvidenceRetireFailure {
                error: BlkError::InvalidDmaProof,
                permit: self,
            });
        }
        if let Err(error) = retire(self.evidence, self.quiesce_epoch) {
            return Err(RecoveryEvidenceRetireFailure {
                error,
                permit: self,
            });
        }
        Ok(RecoveryEvidenceRetired {
            controller_identity: self.controller_identity,
            evidence: self.evidence,
            quiesce_epoch: self.quiesce_epoch,
        })
    }
}

/// Terminal receipt that the runtime latch and exact driver-ledger identity
/// were both retired under the same controller quiescence epoch.
#[derive(Debug, Eq, PartialEq)]
#[must_use = "retain this receipt until request/DMA reclaim consumes its ordering proof"]
pub struct RecoveryEvidenceRetired {
    controller_identity: NonZeroUsize,
    evidence: IrqEvidenceId,
    quiesce_epoch: ControllerEpoch,
}

impl RecoveryEvidenceRetired {
    pub const fn controller_identity(&self) -> usize {
        self.controller_identity.get()
    }

    pub const fn source(&self) -> IrqSourceId {
        self.evidence.source()
    }

    pub const fn evidence_id(&self) -> IrqEvidenceId {
        self.evidence
    }

    pub const fn quiesce_epoch(&self) -> ControllerEpoch {
        self.quiesce_epoch
    }
}

/// Failed driver-ledger retirement retaining the exact linear permission.
#[derive(Debug)]
#[must_use = "retry with the same driver owner or quarantine the retained permission"]
pub struct RecoveryEvidenceRetireFailure {
    error: BlkError,
    permit: RecoveryEvidenceRetirePermit,
}

impl RecoveryEvidenceRetireFailure {
    pub const fn error(&self) -> BlkError {
        self.error
    }

    pub fn into_parts(self) -> (BlkError, RecoveryEvidenceRetirePermit) {
        (self.error, self.permit)
    }
}

impl fmt::Display for RecoveryEvidenceRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl core::error::Error for RecoveryEvidenceRetireFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Result of retiring one source latch after controller DMA quiescence.
#[derive(Debug, Eq, PartialEq)]
pub enum QuiescedEvidenceCompletion {
    /// Runtime source ownership is retired; the driver ledger must consume the
    /// returned controller/source/evidence/epoch-bound permission.
    Complete {
        permit: RecoveryEvidenceRetirePermit,
    },
    /// A capture raced the first clear and the same owner needs another pass.
    Redeliver(QuiescedEvidence),
}

/// Proof that one exact evidence entry has been drained by the driver.
#[derive(Debug, Eq, PartialEq)]
pub struct DrainedEvidence(PendingBlockIrq);

impl DrainedEvidence {
    /// Clears the source latch or returns the same owner for another pass.
    pub fn complete(
        self,
        latch: Pin<&EvidenceLatch>,
    ) -> Result<EvidenceCompletion, (Self, EvidenceLatchError)> {
        let completion = match latch.complete_claim(&self.0.claim) {
            Ok(completion) => completion,
            Err(error) => return Err((self, error)),
        };
        let PendingBlockIrq {
            claim,
            source_epoch,
        } = self.0;
        match completion {
            LatchCompletion::Clean(masked) => {
                let evidence = claim.evidence;
                let permit = masked.map(|masked| RearmPermit {
                    evidence,
                    source_epoch,
                    masked,
                });
                Ok(EvidenceCompletion::Complete { evidence, permit })
            }
            LatchCompletion::Redeliver(claim) => {
                Ok(EvidenceCompletion::Redeliver(PendingBlockIrq {
                    claim,
                    source_epoch,
                }))
            }
        }
    }
}

/// Result of the source-latch clear-and-recheck transition.
#[derive(Debug, Eq, PartialEq)]
pub enum EvidenceCompletion {
    Complete {
        evidence: IrqEvidenceId,
        permit: Option<RearmPermit>,
    },
    Redeliver(PendingBlockIrq),
}

/// Linear permission to rearm the exact source masked by one drained event.
#[derive(Debug, Eq, PartialEq)]
pub struct RearmPermit {
    evidence: IrqEvidenceId,
    source_epoch: IrqEventEpoch,
    masked: MaskedSource,
}

impl RearmPermit {
    pub const fn evidence_id(&self) -> IrqEvidenceId {
        self.evidence
    }

    pub const fn source_epoch(&self) -> IrqEventEpoch {
        self.source_epoch
    }

    /// Attempts to rearm the exact masked source and consumes this permission.
    ///
    /// Failure returns both the precise driver error and the original linear
    /// permission, so recovery or quarantine cannot accidentally lose it.
    pub fn rearm<C>(self, control: &mut C) -> Result<IrqEvidenceId, RearmFailure<C::Error>>
    where
        C: IrqSourceControl + ?Sized,
    {
        match control.rearm(self.masked) {
            Ok(()) => Ok(self.evidence),
            Err(error) => Err(RearmFailure {
                permit: self,
                error,
            }),
        }
    }

    /// Retires a masked-source permission after matching DMA quiescence.
    ///
    /// Recovery and terminal shutdown must not silently drop a permit that
    /// would otherwise authorize source rearm. This explicit terminal path
    /// consumes it only after the retained controller identity is proven
    /// quiescent.
    pub fn retire_after_quiesce(
        self,
        proof: &DmaQuiesced,
        controller_cookie: usize,
    ) -> Result<IrqEvidenceId, RearmRetireFailure> {
        if proof.controller_cookie() != controller_cookie {
            return Err(RearmRetireFailure {
                permit: self,
                error: RearmRetireError::ForeignController,
            });
        }
        Ok(self.evidence)
    }
}

/// Failed source rearm retaining the exact linear permission.
#[derive(Debug)]
pub struct RearmFailure<E> {
    permit: RearmPermit,
    error: E,
}

impl<E> RearmFailure<E> {
    pub const fn error(&self) -> &E {
        &self.error
    }

    pub fn into_parts(self) -> (RearmPermit, E) {
        (self.permit, self.error)
    }
}

/// Why a one-shot source-rearm owner cannot be retired during recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RearmRetireError {
    #[error("DMA-quiescence proof belongs to another controller")]
    ForeignController,
}

/// Failed terminal retirement retaining the exact source-rearm permission.
#[derive(Debug)]
#[must_use = "retry with the matching proof or retain the permit in quarantine"]
pub struct RearmRetireFailure {
    permit: RearmPermit,
    error: RearmRetireError,
}

impl RearmRetireFailure {
    pub const fn error(&self) -> RearmRetireError {
        self.error
    }

    pub fn into_parts(self) -> (RearmPermit, RearmRetireError) {
        (self.permit, self.error)
    }
}

impl fmt::Display for RearmRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl core::error::Error for RearmRetireFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Result of a bounded driver pass over one linear IRQ evidence owner.
#[derive(Debug, Eq, PartialEq)]
pub enum IrqServiceDecision {
    Drained(DrainedEvidence),
    Retained(PendingBlockIrq),
    Recover {
        evidence: PendingBlockIrq,
        fault: ControllerFault,
    },
}

impl IrqServiceDecision {
    pub const fn evidence_id(&self) -> IrqEvidenceId {
        match self {
            Self::Drained(evidence) => evidence.0.claim.evidence,
            Self::Retained(evidence) | Self::Recover { evidence, .. } => evidence.claim.evidence,
        }
    }

    pub const fn is_drained(&self) -> bool {
        matches!(self, Self::Drained(_))
    }
}

/// Portable hardware-facing reason why an evidence owner requires recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ControllerFault {
    #[error("controller lost acknowledged IRQ evidence")]
    LostIrqEvidence,
    #[error("controller protocol state is invalid")]
    Protocol,
    #[error("controller DMA ownership cannot be proven")]
    Dma,
    #[error("controller endpoint ownership is invalid")]
    Ownership,
}

/// Invalid source-latch state or evidence identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EvidenceLatchError {
    #[error("source evidence latch transition remained contended")]
    TransitionContended,
    #[error("source evidence latch is faulted")]
    Faulted,
    #[error("source evidence latch has an invalid state")]
    InvalidState,
    #[error("source evidence latch sequence was exhausted")]
    SequenceExhausted,
    #[error("evidence claim belongs to another source latch")]
    ForeignClaim,
    #[error("evidence claim is stale")]
    StaleClaim,
    #[error("source latch is bound to {configured:?}, not captured source {captured:?}")]
    WrongSource {
        configured: IrqSourceId,
        captured: IrqSourceId,
    },
    #[error("IRQ evidence generation {evidence} does not match masked-source generation {masked}")]
    MaskLifecycleGenerationMismatch { evidence: u64, masked: u64 },
    #[error(
        "masked source identity changed from lifecycle {active_lifecycle}/epoch {active_epoch} to \
         lifecycle {captured_lifecycle}/epoch {captured_epoch}"
    )]
    ConflictingMaskIdentity {
        active_lifecycle: u64,
        active_epoch: u64,
        captured_lifecycle: u64,
        captured_epoch: u64,
    },
    #[error("source captured {captured:?} while {active:?} remained outstanding")]
    ConflictingEvidence {
        active: IrqEvidenceId,
        captured: IrqEvidenceId,
    },
}

/// Invalid identity or generation at the IRQ evidence boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EvidenceError {
    #[error("block IRQ source ID {value} is outside 0..64")]
    InvalidSourceId { value: usize },
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::{ControllerEpoch, DmaQuiesced};

    #[test]
    fn capture_during_completion_marks_the_claim_dirty_without_waiting() {
        let source = IrqSourceId::new(3).unwrap();
        let latch = Box::pin(EvidenceLatch::new(source));
        let evidence = IrqEvidenceId::new(
            source,
            NonZeroU64::new(7).unwrap(),
            11,
            NonZeroU32::new(13).unwrap(),
        );
        let EvidenceClaim::Claimed(claim) = latch.as_ref().claim(evidence, None).unwrap() else {
            unreachable!()
        };

        assert_eq!(
            latch.as_ref().get_ref().state.compare_exchange(
                LATCH_ACTIVE,
                LATCH_COMPLETING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ),
            Ok(LATCH_ACTIVE)
        );
        let masked = MaskedSource::try_new_with_epoch(7, 17, 1 << 3).unwrap();
        assert_eq!(
            latch.as_ref().claim(evidence, Some(masked)),
            Ok(EvidenceClaim::Coalesced)
        );

        let LatchCompletion::Redeliver(redelivered) =
            latch.as_ref().complete_claim(&claim).unwrap()
        else {
            panic!("completion raced by IRQ evidence must redeliver")
        };
        let LatchCompletion::Clean(final_mask) =
            latch.as_ref().complete_claim(&redelivered).unwrap()
        else {
            panic!("a clean second pass must finish the claim")
        };
        assert_eq!(final_mask, Some(masked));
    }

    #[test]
    fn dma_quiescence_retires_recovery_evidence_without_rearming() {
        let source = IrqSourceId::new(5).unwrap();
        let latch = Box::pin(EvidenceLatch::new(source));
        let evidence = IrqEvidenceId::new(
            source,
            NonZeroU64::new(9).unwrap(),
            3,
            NonZeroU32::new(4).unwrap(),
        );
        let masked = MaskedSource::try_new_with_epoch(9, 2, 1 << source.get()).unwrap();
        let EvidenceClaim::Claimed(claim) = latch.as_ref().claim(evidence, Some(masked)).unwrap()
        else {
            unreachable!()
        };
        let pending = PendingBlockIrq::from_claim(claim, IrqEventEpoch::new(7).unwrap());
        // SAFETY: this pure state-machine test models a controller whose IRQ
        // action and DMA engine have already been synchronized and quiesced.
        let proof = unsafe { DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7) };

        let failure = pending.retire_after_quiesce(&proof, 0xdead).unwrap_err();
        let (error, pending) = failure.into_parts();
        assert_eq!(error, EvidenceRetireError::ForeignController);

        let retired = pending
            .retire_after_quiesce(&proof, 0x51a7)
            .expect("the matching DMA proof must retain the evidence owner");
        let QuiescedEvidenceCompletion::Complete { permit } =
            retired.complete(latch.as_ref()).unwrap()
        else {
            panic!("a synchronized source cannot redeliver evidence")
        };
        assert_eq!(permit.evidence_id(), evidence);
        assert_eq!(permit.controller_identity(), 0x51a7);
        assert_eq!(permit.quiesce_epoch(), ControllerEpoch::new(2));

        let EvidenceClaim::Claimed(_) = latch.as_ref().claim(evidence, None).unwrap() else {
            panic!("retiring recovery evidence must release the source latch")
        };
    }
}
