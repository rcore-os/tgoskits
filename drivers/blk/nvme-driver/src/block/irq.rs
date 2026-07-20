//! Hard-IRQ completion capture and logical-vector topology.

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    array,
    num::NonZeroU64,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use rdif_block::{
    BlkError, BlockIrqCapture, BlockIrqSource, ContainmentCause, Event, FaultContainment, IdList,
    InitError, IrqCapture, IrqControlError, IrqEndpoint, IrqSourceControl, IrqSourceInfo,
    IrqSourceList, IrqSourceMaskState, IrqSourceState, MaskedSource,
};

use crate::nvme::NvmeInterruptPort;

const INITIAL_IRQ_SOURCE_GENERATION: u64 = 1;

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
    source_generations: [AtomicU64; u64::BITS as usize],
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

pub(super) fn vector_for_queue(
    msix_interrupts: bool,
    vectors: &[u16],
    queue_id: usize,
) -> Option<u16> {
    if msix_interrupts {
        vectors.get(queue_id).copied()
    } else {
        Some(0)
    }
}

pub(super) fn queue_interrupt_sources(
    msix_interrupts: bool,
    vectors: &[u16],
    queue_id: usize,
) -> IdList {
    let mut sources = IdList::none();
    if let Some(source_id) = vector_for_queue(msix_interrupts, vectors, queue_id) {
        sources.insert(usize::from(source_id));
    }
    sources
}

pub(super) fn source_queue_bits(
    msix_interrupts: bool,
    vectors: &[u16],
    source_id: usize,
    queue_bits: u64,
) -> u64 {
    if !msix_interrupts {
        return if source_id == 0 { queue_bits } else { 0 };
    }

    let mut bits = 0;
    for queue_id in 0..u64::BITS as usize {
        if queue_bits & (1 << queue_id) == 0 {
            continue;
        }
        if vector_for_queue(msix_interrupts, vectors, queue_id) == Some(source_id as u16) {
            bits |= 1 << queue_id;
        }
    }
    bits
}

pub(super) fn irq_sources_from_queue_bits(
    msix_interrupts: bool,
    vectors: &[u16],
    queue_bits: u64,
) -> IrqSourceList {
    if !msix_interrupts {
        return vec![IrqSourceInfo::legacy(IdList::from_bits(queue_bits))];
    }

    let mut sources = Vec::new();
    for vector in unique_interrupt_vectors(vectors) {
        let queues = source_queue_bits(msix_interrupts, vectors, usize::from(vector), queue_bits);
        if queues != 0 {
            sources.push(IrqSourceInfo::new(
                usize::from(vector),
                IdList::from_bits(queues),
            ));
        }
    }
    sources
}

pub(super) fn unique_interrupt_vectors(vectors: &[u16]) -> Vec<u16> {
    let mut unique = Vec::new();
    for vector in vectors {
        if !unique.contains(vector) {
            unique.push(*vector);
        }
    }
    unique
}

impl NvmeIrqState {
    pub(super) fn new(
        interrupt_port: NvmeInterruptPort,
        vectors: &[u16],
        msix_interrupts: bool,
    ) -> Self {
        let configured_source_bits = vectors.iter().fold(0_u64, |bits, vector| {
            let source_id = usize::from(*vector);
            if source_id < u32::BITS as usize {
                bits | (1_u64 << source_id)
            } else {
                bits
            }
        });
        Self {
            interrupt_port,
            masking: if msix_interrupts {
                NvmeInterruptMasking::ExternalMsix
            } else {
                NvmeInterruptMasking::Controller
            },
            configured_source_bits,
            delivery_enabled: AtomicBool::new(false),
            io_armed: AtomicBool::new(false),
            queue_source_taken_bits: AtomicU64::new(0),
            queue_source_live_bits: AtomicU64::new(0),
            source_masked_bits: AtomicU64::new(0),
            source_generations: array::from_fn(|_| AtomicU64::new(INITIAL_IRQ_SOURCE_GENERATION)),
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
        let _ = self.advance_source_generation(source_id);
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
        let _ = self.advance_source_generation(source_id);
        self.queue_source_live_bits
            .fetch_or(source_bit, Ordering::Release);
        true
    }

    pub(super) fn all_queue_sources_live(&self, required_sources: u64) -> bool {
        required_sources != 0
            && required_sources & !self.queue_source_live_bits.load(Ordering::Acquire) == 0
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
            let _ = self.advance_source_generation(source_id);
        });
    }

    fn release_initial_source(&self, source_id: usize) {
        if self.configured_source_bit(source_id).is_some() {
            self.initial_source_live.store(false, Ordering::Release);
            let _ = self.advance_source_generation(source_id);
        }
    }

    fn release_queue_source(&self, source_id: usize) {
        let Some(source_bit) = self.configured_source_bit(source_id) else {
            return;
        };
        self.queue_source_live_bits
            .fetch_and(!source_bit, Ordering::Release);
        let _ = self.advance_source_generation(source_id);
    }

    fn mask_source(&self, source_id: usize) -> Result<MaskedSource, BlkError> {
        if self.masking == NvmeInterruptMasking::ExternalMsix {
            return Err(BlkError::NotSupported);
        }
        let source_bit = self
            .configured_source_bit(source_id)
            .ok_or(BlkError::NotSupported)?;
        self.interrupt_port.mask(source_id as u32);
        self.source_masked_bits
            .fetch_or(source_bit, Ordering::Release);
        let generation = self
            .source_generation(source_id)
            .ok_or(BlkError::NotSupported)?;
        let bitmap =
            NonZeroU64::new(source_bit).expect("a validated NVMe IRQ source has a nonzero bitmap");
        Ok(MaskedSource::new(generation, bitmap))
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
        let active = self
            .source_generation(source_id)
            .ok_or(IrqControlError::Offline)?;
        validate_irq_source_token(source_bit, active, source)?;
        let bitmap = source.bitmap().get();
        let actual = source.generation().get();
        if !self.delivery_enabled() {
            return Err(IrqControlError::Offline);
        }

        let masked = self
            .source_masked_bits
            .fetch_and(!source_bit, Ordering::AcqRel);
        if masked & source_bit == 0 {
            return Err(IrqControlError::SourceNotMasked { bitmap });
        }

        let current = self
            .source_generation(source_id)
            .ok_or(IrqControlError::Offline)?
            .get();
        if current != actual || !self.delivery_enabled() {
            self.source_masked_bits
                .fetch_or(source_bit, Ordering::Release);
            return if current != actual {
                Err(IrqControlError::StaleGeneration {
                    expected: current,
                    actual,
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
        let generation = self.source_generation(source_id)?;
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
        if source_id >= u32::BITS as usize {
            return None;
        }
        let source_bit = 1_u64 << source_id;
        (self.configured_source_bits & source_bit != 0).then_some(source_bit)
    }

    fn source_generation(&self, source_id: usize) -> Option<NonZeroU64> {
        NonZeroU64::new(
            self.source_generations
                .get(source_id)?
                .load(Ordering::Acquire),
        )
    }

    fn advance_source_generation(&self, source_id: usize) -> Option<NonZeroU64> {
        let generation = self.source_generations.get(source_id)?;
        let mut current = generation.load(Ordering::Acquire);
        loop {
            let next = next_irq_source_generation(current);
            match generation.compare_exchange_weak(
                current,
                next.get(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.capture_counts[source_id].store(0, Ordering::Relaxed);
                    self.successful_rearm_counts[source_id].store(0, Ordering::Relaxed);
                    self.failed_rearm_counts[source_id].store(0, Ordering::Relaxed);
                    return Some(next);
                }
                Err(observed) => current = observed,
            }
        }
    }

    fn for_each_source(&self, source_bits: u64, mut visit: impl FnMut(usize)) {
        for source_id in 0..u32::BITS as usize {
            if source_bits & (1_u64 << source_id) != 0 {
                visit(source_id);
            }
        }
    }
}

fn next_irq_source_generation(current: u64) -> NonZeroU64 {
    let next = match current.wrapping_add(1) {
        0 => INITIAL_IRQ_SOURCE_GENERATION,
        next => next,
    };
    NonZeroU64::new(next).expect("NVMe IRQ source generation never advances to zero")
}

fn validate_irq_source_token(
    source_bit: u64,
    active_generation: NonZeroU64,
    source: MaskedSource,
) -> Result<(), IrqControlError> {
    let bitmap = source.bitmap().get();
    if bitmap != source_bit {
        return Err(IrqControlError::SourceNotMasked { bitmap });
    }
    let expected = active_generation.get();
    let actual = source.generation().get();
    if actual != expected {
        return Err(IrqControlError::StaleGeneration { expected, actual });
    }
    Ok(())
}

pub(super) fn new_initial_irq_source(irq: Arc<NvmeIrqState>, source_id: usize) -> BlockIrqSource {
    new_irq_source(
        irq,
        source_id,
        Event::none(),
        IrqEndpointRole::Initialization,
    )
}

pub(super) fn new_queue_irq_source(
    irq: Arc<NvmeIrqState>,
    source_id: usize,
    queues: IdList,
) -> BlockIrqSource {
    new_irq_source(
        irq,
        source_id,
        Event::from_queue_bits(queues.bits()),
        IrqEndpointRole::NormalIo,
    )
}

fn new_irq_source(
    irq: Arc<NvmeIrqState>,
    source_id: usize,
    event: Event,
    role: IrqEndpointRole,
) -> BlockIrqSource {
    BlockIrqSource::new(
        Box::new(NvmeIrqEndpoint {
            irq: Arc::clone(&irq),
            source_id,
            event,
            role,
        }),
        Box::new(NvmeIrqControl { irq, source_id }),
    )
}

struct NvmeIrqEndpoint {
    irq: Arc<NvmeIrqState>,
    source_id: usize,
    event: Event,
    role: IrqEndpointRole,
}

impl IrqEndpoint for NvmeIrqEndpoint {
    type Event = Event;
    type Fault = BlkError;

    fn capture(&mut self) -> BlockIrqCapture {
        if !self.irq.delivery_enabled() {
            return IrqCapture::Unhandled;
        }
        self.irq.record_capture(self.source_id);
        if self.irq.masking == NvmeInterruptMasking::ExternalMsix {
            IrqCapture::Captured {
                event: self.event,
                masked: None,
            }
        } else {
            captured_and_masked(&self.irq, self.source_id, self.event)
        }
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

fn captured_and_masked(irq: &NvmeIrqState, source_id: usize, event: Event) -> BlockIrqCapture {
    match irq.mask_source(source_id) {
        Ok(masked) => IrqCapture::Captured {
            event,
            masked: Some(masked),
        },
        Err(reason) => IrqCapture::Fault {
            reason,
            containment: FaultContainment::Uncontained,
        },
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use rdif_block::{IrqControlError, MaskedSource};

    use super::{next_irq_source_generation, validate_irq_source_token};

    #[test]
    fn irq_source_generation_wraps_without_using_zero() {
        assert_eq!(next_irq_source_generation(1).get(), 2);
        assert_eq!(next_irq_source_generation(u64::MAX).get(), 1);
    }

    #[test]
    fn rearm_token_is_bound_to_one_source_and_generation() {
        let active = NonZeroU64::new(7).unwrap();
        let matching = MaskedSource::try_new(7, 1 << 3).unwrap();
        assert_eq!(validate_irq_source_token(1 << 3, active, matching), Ok(()));

        let stale = MaskedSource::try_new(6, 1 << 3).unwrap();
        assert!(matches!(
            validate_irq_source_token(1 << 3, active, stale),
            Err(IrqControlError::StaleGeneration { .. })
        ));

        let wrong_source = MaskedSource::try_new(7, 1 << 4).unwrap();
        assert_eq!(
            validate_irq_source_token(1 << 3, active, wrong_source),
            Err(IrqControlError::SourceNotMasked { bitmap: 1 << 4 })
        );
    }
}
