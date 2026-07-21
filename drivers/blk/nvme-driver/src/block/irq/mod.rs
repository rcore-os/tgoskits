//! Hard-IRQ completion capture and logical-vector topology.

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    array,
    num::NonZeroU64,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use rdif_block::{
    BlkError, BlockEvidenceSource, BlockIrqCapture, BlockIrqSource, ContainmentCause, Event,
    FaultContainment, IdList, InitError, IrqCapture, IrqControlError, IrqEndpoint, IrqEvidenceId,
    IrqSourceControl, IrqSourceId, IrqSourceInfo, IrqSourceList, IrqSourceMaskState,
    IrqSourceState, MaskedSource,
};

use super::evidence_ledger::{NvmeEvidenceError, NvmeEvidenceFacts, NvmeEvidenceLedger};
use crate::{nvme::NvmeInterruptPort, queue::NvmeCompletionProbe};

const INITIAL_IRQ_LIFECYCLE_GENERATION: u64 = 1;
const INITIAL_IRQ_MASK_EPOCH: u64 = 1;

mod topology;

pub(in crate::block) use topology::*;

/// IRQ-only NVMe capability shared by registered source endpoints.
///
/// This object intentionally contains no admin queue, I/O completion queue,
/// request state, or controller state machine. A hard-IRQ callback can only
/// mask its exact vector and publish the immutable queue routing captured when
/// the action was registered.
pub(super) struct NvmeIrqState {
    interrupt_port: NvmeInterruptPort,
    masking: NvmeInterruptMasking,
    configured_source_bits: u64,
    delivery_enabled: AtomicBool,
    io_armed: AtomicBool,
    queue_source_taken_bits: AtomicU64,
    queue_source_live_bits: AtomicU64,
    source_masked_bits: AtomicU64,
    source_lifecycle_generations: [AtomicU64; u64::BITS as usize],
    source_mask_epochs: [AtomicU64; u64::BITS as usize],
    capture_counts: [AtomicU64; u64::BITS as usize],
    successful_rearm_counts: [AtomicU64; u64::BITS as usize],
    failed_rearm_counts: [AtomicU64; u64::BITS as usize],
    initial_source_taken: AtomicBool,
    initial_source_live: AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NvmeInterruptMasking {
    /// Pin-based and MSI delivery use the controller INTMS/INTMC registers.
    Controller,
    /// MSI-X delivery is masked only through the PCI MSI-X table owner.
    ExternalMsix,
}

impl NvmeIrqState {
    pub(super) fn new(
        interrupt_port: NvmeInterruptPort,
        vectors: &[u16],
        msix_interrupts: bool,
    ) -> Self {
        let configured_source_bits = vectors.iter().fold(0_u64, |bits, vector| {
            let source_id = usize::from(*vector);
            if source_id < u64::BITS as usize {
                bits | (1_u64 << source_id)
            } else {
                bits
            }
        });
        let masking = if msix_interrupts {
            NvmeInterruptMasking::ExternalMsix
        } else {
            NvmeInterruptMasking::Controller
        };
        Self {
            interrupt_port,
            masking,
            configured_source_bits,
            delivery_enabled: AtomicBool::new(false),
            io_armed: AtomicBool::new(false),
            queue_source_taken_bits: AtomicU64::new(0),
            queue_source_live_bits: AtomicU64::new(0),
            // Discovery leaves every controller-owned source masked. MSI-X
            // masking belongs to its external PCI vector lease instead.
            source_masked_bits: AtomicU64::new(if masking == NvmeInterruptMasking::Controller {
                configured_source_bits
            } else {
                0
            }),
            source_lifecycle_generations: array::from_fn(|_| {
                AtomicU64::new(INITIAL_IRQ_LIFECYCLE_GENERATION)
            }),
            source_mask_epochs: array::from_fn(|_| AtomicU64::new(INITIAL_IRQ_MASK_EPOCH)),
            capture_counts: array::from_fn(|_| AtomicU64::new(0)),
            successful_rearm_counts: array::from_fn(|_| AtomicU64::new(0)),
            failed_rearm_counts: array::from_fn(|_| AtomicU64::new(0)),
            initial_source_taken: AtomicBool::new(false),
            initial_source_live: AtomicBool::new(false),
        }
    }

    pub(super) fn delivery_enabled(&self) -> bool {
        self.delivery_enabled.load(Ordering::Acquire)
    }

    pub(super) fn enable_delivery(&self) {
        self.delivery_enabled.store(true, Ordering::Release);
    }

    pub(super) fn initial_source_live(&self) -> bool {
        self.initial_source_live.load(Ordering::Acquire)
    }

    pub(super) fn take_initial_source(&self, source_id: usize) -> bool {
        if self.configured_source_bit(source_id).is_none()
            || self.initial_source_taken.swap(true, Ordering::AcqRel)
        {
            return false;
        }
        let _ = self.advance_source_lifecycle(source_id);
        self.initial_source_live.store(true, Ordering::Release);
        true
    }

    pub(super) fn any_queue_source_taken(&self) -> bool {
        self.queue_source_taken_bits.load(Ordering::Acquire) != 0
    }

    pub(super) fn take_queue_source(&self, source_id: usize) -> bool {
        let Some(source_bit) = self.configured_source_bit(source_id) else {
            return false;
        };
        if self
            .queue_source_taken_bits
            .fetch_or(source_bit, Ordering::AcqRel)
            & source_bit
            != 0
        {
            return false;
        }
        let _ = self.advance_source_lifecycle(source_id);
        self.queue_source_live_bits
            .fetch_or(source_bit, Ordering::Release);
        true
    }

    pub(super) fn all_queue_sources_live(&self, required_sources: u64) -> bool {
        required_sources != 0
            && required_sources & !self.queue_source_live_bits.load(Ordering::Acquire) == 0
    }

    pub(super) fn queue_source_live(&self, source_id: usize) -> bool {
        self.configured_source_bit(source_id)
            .is_some_and(|source_bit| {
                self.queue_source_live_bits.load(Ordering::Acquire) & source_bit != 0
            })
    }

    pub(super) fn arm_io_sources(&self, source_bits: u64) {
        self.delivery_enabled.store(true, Ordering::Release);
        if self.io_armed.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.masking == NvmeInterruptMasking::ExternalMsix {
            return;
        }
        self.source_masked_bits
            .fetch_and(!source_bits, Ordering::Release);
        self.for_each_source(source_bits, |source_id| {
            self.interrupt_port.unmask(source_id as u32);
        });
    }

    pub(super) fn unmask_for_activation(&self, source_id: usize) -> Result<(), InitError> {
        let source_bit = self
            .configured_source_bit(source_id)
            .ok_or(InitError::MissingInterrupt)?;
        if self.masking == NvmeInterruptMasking::ExternalMsix {
            return Ok(());
        }
        self.source_masked_bits
            .fetch_and(!source_bit, Ordering::Release);
        self.interrupt_port.unmask(source_id as u32);
        Ok(())
    }

    pub(super) fn disable_all(&self) {
        if self.masking == NvmeInterruptMasking::Controller {
            self.for_each_source(self.configured_source_bits, |source_id| {
                self.interrupt_port.mask(source_id as u32);
            });
        }
        self.delivery_enabled.store(false, Ordering::Release);
        self.io_armed.store(false, Ordering::Release);
        if self.masking == NvmeInterruptMasking::Controller {
            self.source_masked_bits
                .fetch_or(self.configured_source_bits, Ordering::Release);
        }
        self.for_each_source(self.configured_source_bits, |source_id| {
            let _ = self.advance_source_lifecycle(source_id);
        });
    }

    fn release_initial_source(&self, source_id: usize) {
        if self.configured_source_bit(source_id).is_some() {
            self.initial_source_live.store(false, Ordering::Release);
            let _ = self.advance_source_lifecycle(source_id);
        }
    }

    fn release_queue_source(&self, source_id: usize) {
        let Some(source_bit) = self.configured_source_bit(source_id) else {
            return;
        };
        self.queue_source_live_bits
            .fetch_and(!source_bit, Ordering::Release);
        let _ = self.advance_source_lifecycle(source_id);
    }

    fn capture_mask_source(&self, source_id: usize) -> Result<Option<MaskedSource>, BlkError> {
        if self.masking == NvmeInterruptMasking::ExternalMsix {
            return Err(BlkError::NotSupported);
        }
        let source_bit = self
            .configured_source_bit(source_id)
            .ok_or(BlkError::NotSupported)?;
        // The registered callback and its maintenance owner are fixed to one
        // CPU, and the callback is non-reentrant. This atomic transition is
        // still the publication point: only its winner may advance the mask
        // epoch and let a new token escape. A shared-line peer may observe an
        // already-masked source and merge facts, but receives no second token.
        let previously_masked = self
            .source_masked_bits
            .fetch_or(source_bit, Ordering::AcqRel)
            & source_bit
            != 0;
        if previously_masked {
            return Ok(None);
        }
        self.interrupt_port.mask(source_id as u32);
        let lifecycle_generation = self
            .source_lifecycle_generation(source_id)
            .ok_or(BlkError::NotSupported)?;
        let mask_epoch = self
            .advance_source_mask_epoch(source_id)
            .ok_or(BlkError::NotSupported)?;
        let bitmap =
            NonZeroU64::new(source_bit).expect("a validated NVMe IRQ source has a nonzero bitmap");
        Ok(Some(MaskedSource::new_with_epoch(
            lifecycle_generation,
            mask_epoch,
            bitmap,
        )))
    }

    fn mask_source(&self, source_id: usize) -> Result<MaskedSource, BlkError> {
        self.capture_mask_source(source_id)?.ok_or(BlkError::Busy)
    }

    fn rearm_source(&self, source_id: usize, source: MaskedSource) -> Result<(), IrqControlError> {
        let result = self.try_rearm_source(source_id, source);
        if let Some(counter) = match &result {
            Ok(()) => self.successful_rearm_counts.get(source_id),
            Err(_) => self.failed_rearm_counts.get(source_id),
        } {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    fn try_rearm_source(
        &self,
        source_id: usize,
        source: MaskedSource,
    ) -> Result<(), IrqControlError> {
        if self.masking == NvmeInterruptMasking::ExternalMsix {
            return Err(IrqControlError::SourceNotMasked {
                bitmap: source.bitmap().get(),
            });
        }
        let source_bit =
            self.configured_source_bit(source_id)
                .ok_or(IrqControlError::SourceNotMasked {
                    bitmap: source.bitmap().get(),
                })?;
        let active_lifecycle = self
            .source_lifecycle_generation(source_id)
            .ok_or(IrqControlError::Offline)?;
        let active_mask_epoch = self
            .source_mask_epoch(source_id)
            .ok_or(IrqControlError::Offline)?;
        validate_irq_source_token(source_bit, active_lifecycle, active_mask_epoch, source)?;
        let bitmap = source.bitmap().get();
        let actual_lifecycle = source.lifecycle_generation().get();
        let actual_mask_epoch = source.mask_epoch().get();
        if !self.delivery_enabled() {
            return Err(IrqControlError::Offline);
        }

        let masked = self
            .source_masked_bits
            .fetch_and(!source_bit, Ordering::AcqRel);
        if masked & source_bit == 0 {
            return Err(IrqControlError::SourceNotMasked { bitmap });
        }

        let current_lifecycle = self
            .source_lifecycle_generation(source_id)
            .ok_or(IrqControlError::Offline)?
            .get();
        let current_mask_epoch = self
            .source_mask_epoch(source_id)
            .ok_or(IrqControlError::Offline)?
            .get();
        if current_lifecycle != actual_lifecycle
            || current_mask_epoch != actual_mask_epoch
            || !self.delivery_enabled()
        {
            self.source_masked_bits
                .fetch_or(source_bit, Ordering::Release);
            return if current_lifecycle != actual_lifecycle {
                Err(IrqControlError::StaleGeneration {
                    expected: current_lifecycle,
                    actual: actual_lifecycle,
                })
            } else if current_mask_epoch != actual_mask_epoch {
                Err(IrqControlError::StaleMaskEpoch {
                    expected: current_mask_epoch,
                    actual: actual_mask_epoch,
                })
            } else {
                Err(IrqControlError::Offline)
            };
        }

        self.interrupt_port.unmask(source_id as u32);
        Ok(())
    }

    fn record_capture(&self, source_id: usize) {
        if let Some(counter) = self.capture_counts.get(source_id) {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn state(&self, source_id: usize) -> Option<IrqSourceState> {
        let source_bit = self.configured_source_bit(source_id)?;
        let generation = self.source_lifecycle_generation(source_id)?;
        let mask = match self.masking {
            NvmeInterruptMasking::ExternalMsix => IrqSourceMaskState::External,
            NvmeInterruptMasking::Controller => {
                if self.source_masked_bits.load(Ordering::Acquire) & source_bit == 0 {
                    IrqSourceMaskState::Armed
                } else {
                    IrqSourceMaskState::Masked
                }
            }
        };
        Some(IrqSourceState::new(
            generation,
            self.delivery_enabled(),
            mask,
            self.capture_counts[source_id].load(Ordering::Relaxed),
            self.successful_rearm_counts[source_id].load(Ordering::Relaxed),
            self.failed_rearm_counts[source_id].load(Ordering::Relaxed),
        ))
    }

    fn configured_source_bit(&self, source_id: usize) -> Option<u64> {
        if source_id >= u64::BITS as usize {
            return None;
        }
        let source_bit = 1_u64 << source_id;
        (self.configured_source_bits & source_bit != 0).then_some(source_bit)
    }

    fn source_lifecycle_generation(&self, source_id: usize) -> Option<NonZeroU64> {
        NonZeroU64::new(
            self.source_lifecycle_generations
                .get(source_id)?
                .load(Ordering::Acquire),
        )
    }

    fn source_mask_epoch(&self, source_id: usize) -> Option<NonZeroU64> {
        NonZeroU64::new(
            self.source_mask_epochs
                .get(source_id)?
                .load(Ordering::Acquire),
        )
    }

    fn advance_source_lifecycle(&self, source_id: usize) -> Option<NonZeroU64> {
        let next = advance_epoch(self.source_lifecycle_generations.get(source_id)?)?;
        self.source_mask_epochs[source_id].store(INITIAL_IRQ_MASK_EPOCH, Ordering::Release);
        self.capture_counts[source_id].store(0, Ordering::Relaxed);
        self.successful_rearm_counts[source_id].store(0, Ordering::Relaxed);
        self.failed_rearm_counts[source_id].store(0, Ordering::Relaxed);
        Some(next)
    }

    fn advance_source_mask_epoch(&self, source_id: usize) -> Option<NonZeroU64> {
        advance_epoch(self.source_mask_epochs.get(source_id)?)
    }

    fn for_each_source(&self, source_bits: u64, mut visit: impl FnMut(usize)) {
        for source_id in 0..u64::BITS as usize {
            if source_bits & (1_u64 << source_id) != 0 {
                visit(source_id);
            }
        }
    }
}

fn advance_epoch(epoch: &AtomicU64) -> Option<NonZeroU64> {
    let mut current = epoch.load(Ordering::Acquire);
    loop {
        let next = next_nonzero_epoch(current);
        match epoch.compare_exchange_weak(current, next.get(), Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return Some(next),
            Err(observed) => current = observed,
        }
    }
}

fn next_nonzero_epoch(current: u64) -> NonZeroU64 {
    let next = match current.wrapping_add(1) {
        0 => INITIAL_IRQ_LIFECYCLE_GENERATION,
        next => next,
    };
    NonZeroU64::new(next).expect("NVMe IRQ source generation never advances to zero")
}

fn validate_irq_source_token(
    source_bit: u64,
    active_lifecycle_generation: NonZeroU64,
    active_mask_epoch: NonZeroU64,
    source: MaskedSource,
) -> Result<(), IrqControlError> {
    let bitmap = source.bitmap().get();
    if bitmap != source_bit {
        return Err(IrqControlError::SourceNotMasked { bitmap });
    }
    let expected = active_lifecycle_generation.get();
    let actual = source.lifecycle_generation().get();
    if actual != expected {
        return Err(IrqControlError::StaleGeneration { expected, actual });
    }
    if source.mask_epoch() != active_mask_epoch {
        return Err(IrqControlError::StaleMaskEpoch {
            expected: active_mask_epoch.get(),
            actual: source.mask_epoch().get(),
        });
    }
    Ok(())
}

pub(super) fn new_initial_irq_source(
    irq: Arc<NvmeIrqState>,
    source_id: usize,
    probe: NvmeCompletionProbe,
) -> BlockIrqSource {
    new_irq_source(
        irq,
        source_id,
        Event::none(),
        vec![probe],
        IrqEndpointRole::Initialization,
    )
}

pub(super) fn new_queue_irq_source(
    irq: Arc<NvmeIrqState>,
    source_id: usize,
    queues: IdList,
    probes: Vec<NvmeCompletionProbe>,
) -> BlockIrqSource {
    new_irq_source(
        irq,
        source_id,
        Event::from_queue_bits(queues.bits()),
        probes,
        IrqEndpointRole::NormalIo,
    )
}

/// Creates one exact-vector v0.13 evidence source.
///
/// Only probes explicitly mapped to `source` are moved into the endpoint. An
/// MSI-X endpoint does not touch controller INTMS/INTMC; its platform vector
/// lease remains the owner of table-entry masking.
pub(super) fn new_vector_evidence_source(
    irq: Arc<NvmeIrqState>,
    source: IrqSourceId,
    ledger_slot: u16,
    admin_probe: Option<NvmeCompletionProbe>,
    queue_probes: Vec<(usize, NvmeCompletionProbe)>,
) -> Result<(BlockEvidenceSource, Arc<NvmeEvidenceLedger>), BlkError> {
    let source_id = source.get();
    if !irq.take_queue_source(source_id) {
        return Err(BlkError::NotSupported);
    }
    let mut queue_bits = 0_u64;
    for (queue_id, _) in &queue_probes {
        if *queue_id >= u64::BITS as usize {
            irq.release_queue_source(source_id);
            return Err(BlkError::InvalidRequest);
        }
        let bit = 1_u64 << queue_id;
        if queue_bits & bit != 0 {
            irq.release_queue_source(source_id);
            return Err(BlkError::InvalidRequest);
        }
        queue_bits |= bit;
    }

    let ledger = Arc::new(NvmeEvidenceLedger::new(source, ledger_slot));
    let endpoint = NvmeEvidenceEndpoint {
        irq: Arc::clone(&irq),
        source,
        admin_probe,
        queue_probes,
        ledger: Arc::clone(&ledger),
    };
    let control = NvmeIrqControl { irq, source_id };
    Ok((
        BlockEvidenceSource::new(Box::new(endpoint), Box::new(control)),
        ledger,
    ))
}

fn new_irq_source(
    irq: Arc<NvmeIrqState>,
    source_id: usize,
    event: Event,
    probes: Vec<NvmeCompletionProbe>,
    role: IrqEndpointRole,
) -> BlockIrqSource {
    BlockIrqSource::new(
        Box::new(NvmeIrqEndpoint {
            irq: Arc::clone(&irq),
            source_id,
            event,
            probes,
            role,
        }),
        Box::new(NvmeIrqControl { irq, source_id }),
    )
}

struct NvmeIrqEndpoint {
    irq: Arc<NvmeIrqState>,
    source_id: usize,
    event: Event,
    probes: Vec<NvmeCompletionProbe>,
    role: IrqEndpointRole,
}

struct NvmeEvidenceEndpoint {
    irq: Arc<NvmeIrqState>,
    source: IrqSourceId,
    admin_probe: Option<NvmeCompletionProbe>,
    queue_probes: Vec<(usize, NvmeCompletionProbe)>,
    ledger: Arc<NvmeEvidenceLedger>,
}

impl IrqEndpoint for NvmeEvidenceEndpoint {
    type Event = IrqEvidenceId;
    type Fault = BlkError;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        if !self.irq.delivery_enabled() {
            return IrqCapture::Unhandled;
        }
        let facts = claim_pending_facts(self.admin_probe.as_ref(), &self.queue_probes);
        if facts.is_empty() {
            return IrqCapture::Unhandled;
        }

        let source_id = self.source.get();
        self.irq.record_capture(source_id);
        let masked = match self.irq.masking {
            NvmeInterruptMasking::Controller => match self.irq.capture_mask_source(source_id) {
                Ok(masked) => masked,
                Err(reason) => {
                    return IrqCapture::Fault {
                        reason,
                        containment: FaultContainment::Uncontained,
                    };
                }
            },
            NvmeInterruptMasking::ExternalMsix => None,
        };
        let lifecycle_generation = masked
            .as_ref()
            .map(|source| source.lifecycle_generation())
            .or_else(|| self.irq.source_lifecycle_generation(source_id))
            .expect("a live NVMe source has a nonzero lifecycle generation");
        match self.ledger.publish_capture(lifecycle_generation, facts) {
            Ok(publication)
                if self.irq.masking == NvmeInterruptMasking::ExternalMsix
                    || publication.is_fresh() == masked.is_some() =>
            {
                IrqCapture::Captured {
                    event: publication.identity(),
                    masked,
                }
            }
            Ok(_publication) => IrqCapture::Fault {
                reason: BlkError::Io,
                containment: masked.map_or(
                    FaultContainment::Uncontained,
                    FaultContainment::DeviceSourceMasked,
                ),
            },
            Err(error) => {
                let containment = masked.map_or(
                    FaultContainment::Uncontained,
                    FaultContainment::DeviceSourceMasked,
                );
                IrqCapture::Fault {
                    reason: evidence_capture_error(error),
                    containment,
                }
            }
        }
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        self.irq.mask_source(self.source.get())
    }
}

impl Drop for NvmeEvidenceEndpoint {
    fn drop(&mut self) {
        self.irq.release_queue_source(self.source.get());
    }
}

impl IrqEndpoint for NvmeIrqEndpoint {
    type Event = Event;
    type Fault = BlkError;

    fn capture(&mut self) -> BlockIrqCapture {
        if !self.irq.delivery_enabled() {
            return IrqCapture::Unhandled;
        }
        capture_if_completion_pending(&self.probes, || {
            self.irq.record_capture(self.source_id);
            if self.irq.masking == NvmeInterruptMasking::ExternalMsix {
                IrqCapture::Captured {
                    event: self.event,
                    masked: None,
                }
            } else {
                capture_with_controller_mask(&self.irq, self.source_id, self.event)
            }
        })
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        self.irq.mask_source(self.source_id)
    }
}

impl Drop for NvmeIrqEndpoint {
    fn drop(&mut self) {
        match self.role {
            IrqEndpointRole::Initialization => self.irq.release_initial_source(self.source_id),
            IrqEndpointRole::NormalIo => self.irq.release_queue_source(self.source_id),
        }
    }
}

struct NvmeIrqControl {
    irq: Arc<NvmeIrqState>,
    source_id: usize,
}

impl IrqSourceControl for NvmeIrqControl {
    type Error = IrqControlError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        self.irq.rearm_source(self.source_id, source)
    }

    fn state(&self) -> Option<IrqSourceState> {
        self.irq.state(self.source_id)
    }
}

#[derive(Clone, Copy)]
enum IrqEndpointRole {
    Initialization,
    NormalIo,
}

fn capture_with_controller_mask(
    irq: &NvmeIrqState,
    source_id: usize,
    event: Event,
) -> BlockIrqCapture {
    match irq.capture_mask_source(source_id) {
        Ok(masked) => IrqCapture::Captured { event, masked },
        Err(reason) => IrqCapture::Fault {
            reason,
            containment: FaultContainment::Uncontained,
        },
    }
}

fn capture_if_completion_pending(
    probes: &[NvmeCompletionProbe],
    capture: impl FnOnce() -> BlockIrqCapture,
) -> BlockIrqCapture {
    let mut pending = false;
    for probe in probes {
        // Do not short-circuit: one shared vector publication claims every CQ
        // already carrying evidence, so peer callbacks coalesce until their
        // owner cursors advance.
        pending |= probe.try_claim_pending();
    }
    if pending {
        capture()
    } else {
        IrqCapture::Unhandled
    }
}

fn claim_pending_facts(
    admin_probe: Option<&NvmeCompletionProbe>,
    queue_probes: &[(usize, NvmeCompletionProbe)],
) -> NvmeEvidenceFacts {
    let mut queue_bits = 0_u64;
    for (queue_id, probe) in queue_probes {
        if probe.try_claim_pending() {
            queue_bits |= 1_u64 << queue_id;
        }
    }
    if admin_probe.is_some_and(NvmeCompletionProbe::try_claim_pending) {
        NvmeEvidenceFacts::queues(queue_bits).with_admin()
    } else {
        NvmeEvidenceFacts::queues(queue_bits)
    }
}

const fn evidence_capture_error(error: NvmeEvidenceError) -> BlkError {
    match error {
        NvmeEvidenceError::GenerationExhausted => BlkError::QueueEpochExhausted,
        NvmeEvidenceError::EmptyFacts
        | NvmeEvidenceError::LifecycleConflict
        | NvmeEvidenceError::IdentityMismatch
        | NvmeEvidenceError::NotPublished
        | NvmeEvidenceError::PublicationInProgress => BlkError::Io,
    }
}

#[cfg(test)]
mod tests;
