//! CV1800 SDIO1 wrappers around the generic SDHCI split IRQ source.

use alloc::sync::Arc;
use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use rdif_irq::{
    ContainmentCause, FaultContainment, IrqCapture, IrqEndpoint, IrqSourceControl, MaskedSource,
};
use sdhci_host::{Event, SDHCI_IRQ_SOURCE_BITMAP, SdhciIrqControl, SdhciIrqEndpoint};
use sdmmc_protocol::{
    Error,
    block::BlockRequestId,
    sdio::{HostEvent, HostEventKind, HostEventSource, HostEventSummary, SdioIrqControlError},
};

use crate::hw_init::Sdio1MappedResources;

/// Stable SDIO1 controller event captured and acknowledged in hard IRQ.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CviSdhciIrqEvent(pub Event);

impl HostEvent for CviSdhciIrqEvent {
    fn kind(&self) -> HostEventKind {
        self.0.kind()
    }

    fn source(&self) -> HostEventSource {
        self.0.source()
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        self.0.queue_id()
    }

    fn requests_block_queue_service(&self) -> bool {
        self.0.requests_block_queue_service()
    }

    fn stable_summary(&self) -> HostEventSummary {
        self.0.stable_summary()
    }
}

/// Move-only destructive status endpoint owned by the OS IRQ action.
pub struct CviSdhciIrqEndpoint {
    pub(crate) inner: SdhciIrqEndpoint,
    pub(crate) card: Arc<CardIrqState>,
    pub(crate) base: usize,
    pub(crate) _resources: Arc<Sdio1MappedResources>,
}

impl IrqEndpoint for CviSdhciIrqEndpoint {
    type Event = CviSdhciIrqEvent;
    type Fault = Error;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        match self.inner.capture() {
            IrqCapture::Unhandled => IrqCapture::Unhandled,
            IrqCapture::Captured { event, masked } => {
                let card_masked = match capture_card_source(&self.card, self.base, event) {
                    Ok(source) => source,
                    Err(reason) => {
                        return IrqCapture::Fault {
                            reason,
                            containment: FaultContainment::Uncontained,
                        };
                    }
                };
                debug_assert!(
                    masked.is_none(),
                    "SDHCI capture never masks its full source"
                );
                IrqCapture::Captured {
                    event: CviSdhciIrqEvent(event),
                    masked: card_masked,
                }
            }
            IrqCapture::Fault {
                reason,
                containment,
            } => IrqCapture::Fault {
                reason,
                containment,
            },
        }
    }

    fn contain(&mut self, cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        disable_card_signal(self.base);
        let source = self.inner.contain(cause)?;
        self.card.mark_full_contained();
        Ok(source)
    }
}

fn capture_card_source(
    card: &CardIrqState,
    base: usize,
    event: Event,
) -> Result<Option<MaskedSource>, Error> {
    if event.normal_status() & NORMAL_INT_CARD == 0 {
        return Ok(None);
    }
    disable_card_signal(base);
    card.mask_card().map(Some)
}

/// Generation-checked source control retained by the maintenance owner.
pub struct CviSdhciIrqControl {
    pub(crate) inner: SdhciIrqControl,
    pub(crate) card: Arc<CardIrqState>,
    pub(crate) base: usize,
    pub(crate) _resources: Arc<Sdio1MappedResources>,
}

impl IrqSourceControl for CviSdhciIrqControl {
    type Error = SdioIrqControlError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        match source.bitmap().get() {
            CARD_IRQ_SOURCE_BITMAP => {
                if self.card.rearm_card(source)? {
                    enable_card_signal(self.base);
                }
                Ok(())
            }
            SDHCI_IRQ_SOURCE_BITMAP => {
                self.inner.rearm(source)?;
                if self.card.finish_full_containment() {
                    enable_card_signal(self.base);
                }
                Ok(())
            }
            bitmap => Err(SdioIrqControlError::SourceNotMasked { bitmap }),
        }
    }
}

pub(crate) const NORMAL_INT_CARD: u16 = 1 << 8;
const CARD_IRQ_SOURCE_BITMAP: u64 = 1 << 1;
const NORMAL_INT_STATUS_ENABLE: usize = 0x34;
const NORMAL_INT_SIGNAL_ENABLE: usize = 0x38;

/// Independent generation and mask ownership for the SDIO CARD_INTERRUPT bit.
///
/// The generic SDHCI source token covers command/data delivery. CARD_INTERRUPT
/// is independently maskable and therefore must not borrow that token: doing
/// so would let a command completion rearm sideband delivery, or vice versa.
pub(crate) struct CardIrqState {
    generation: AtomicU64,
    online: AtomicBool,
    card_masked: AtomicBool,
    full_contained: AtomicBool,
}

impl CardIrqState {
    pub(crate) const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            online: AtomicBool::new(false),
            card_masked: AtomicBool::new(false),
            full_contained: AtomicBool::new(false),
        }
    }

    pub(crate) fn activate(&self) {
        let mut current = self.generation.load(Ordering::Relaxed);
        loop {
            let next = current.wrapping_add(1).max(1);
            match self.generation.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        self.card_masked.store(false, Ordering::Relaxed);
        self.full_contained.store(false, Ordering::Relaxed);
        self.online.store(true, Ordering::Release);
    }

    pub(crate) fn deactivate(&self) {
        self.online.store(false, Ordering::Release);
        self.card_masked.store(false, Ordering::Relaxed);
        self.full_contained.store(false, Ordering::Relaxed);
    }

    fn mask_card(&self) -> Result<MaskedSource, Error> {
        if !self.online.load(Ordering::Acquire) {
            return Err(Error::InvalidArgument);
        }
        let generation = NonZeroU64::new(self.generation.load(Ordering::Acquire))
            .ok_or(Error::InvalidArgument)?;
        self.card_masked.store(true, Ordering::Release);
        Ok(MaskedSource::new(
            generation,
            NonZeroU64::new(CARD_IRQ_SOURCE_BITMAP).expect("CARD source bitmap is nonzero"),
        ))
    }

    fn mark_full_contained(&self) {
        self.full_contained.store(true, Ordering::Release);
    }

    fn rearm_card(&self, source: MaskedSource) -> Result<bool, SdioIrqControlError> {
        let expected = NonZeroU64::new(self.generation.load(Ordering::Acquire))
            .ok_or(SdioIrqControlError::Offline)?;
        let actual = source.generation();
        if actual != expected {
            return Err(SdioIrqControlError::StaleGeneration {
                expected: expected.get(),
                actual: actual.get(),
            });
        }
        if !self.online.load(Ordering::Acquire) {
            return Err(SdioIrqControlError::Offline);
        }
        if !self.card_masked.swap(false, Ordering::AcqRel) {
            return Err(SdioIrqControlError::SourceNotMasked {
                bitmap: CARD_IRQ_SOURCE_BITMAP,
            });
        }
        Ok(!self.full_contained.load(Ordering::Acquire))
    }

    fn finish_full_containment(&self) -> bool {
        self.full_contained.swap(false, Ordering::AcqRel)
            && !self.card_masked.load(Ordering::Acquire)
            && self.online.load(Ordering::Acquire)
    }
}

pub(crate) fn enable_card_status(base: usize) {
    update16(base + NORMAL_INT_STATUS_ENABLE, |value| {
        value | NORMAL_INT_CARD
    });
}

pub(crate) fn enable_card_signal(base: usize) {
    update16(base + NORMAL_INT_SIGNAL_ENABLE, |value| {
        value | NORMAL_INT_CARD
    });
}

fn disable_card_signal(base: usize) {
    update16(base + NORMAL_INT_SIGNAL_ENABLE, |value| {
        value & !NORMAL_INT_CARD
    });
}

fn update16(address: usize, update: impl FnOnce(u16) -> u16) {
    // SAFETY: the split endpoints share the controller mapping lifetime. The
    // owner invokes this only after generic source-generation validation.
    unsafe {
        let value = core::ptr::read_volatile(address as *const u16);
        core::ptr::write_volatile(address as *mut u16, update(value));
    }
}

// Keep the containment type in this module's public design vocabulary: a
// capture fault reports whether the exact device source is already stopped.
const _: Option<FaultContainment> = None;

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    #[test]
    fn combined_command_and_card_event_masks_only_card_source() {
        let mut regs = FakeRegs([0; 0x100]);
        let base = regs.0.as_mut_ptr() as usize;
        write_signal(base, u16::MAX);
        let state = CardIrqState::new();
        state.activate();

        let event = Event::from_status(1 | NORMAL_INT_CARD, 0);
        let source = capture_card_source(&state, base, event)
            .unwrap()
            .expect("CARD_INTERRUPT must return its own masked token");

        assert_eq!(event.normal_status(), 1 | NORMAL_INT_CARD);
        assert_eq!(source.bitmap().get(), CARD_IRQ_SOURCE_BITMAP);
        assert_eq!(read_signal(base) & NORMAL_INT_CARD, 0);
        assert_ne!(read_signal(base) & 1, 0, "command delivery stays armed");
    }

    #[test]
    fn repeated_card_containment_is_idempotent_but_rearm_is_single_use() {
        let state = CardIrqState::new();
        state.activate();
        let first = state.mask_card().unwrap();
        let repeated = state.mask_card().unwrap();
        assert_eq!(first, repeated);

        assert!(state.rearm_card(first).unwrap());
        assert!(matches!(
            state.rearm_card(repeated),
            Err(SdioIrqControlError::SourceNotMasked { bitmap })
                if bitmap == CARD_IRQ_SOURCE_BITMAP
        ));
    }

    #[test]
    fn recovery_generation_rejects_stale_card_rearm() {
        let state = CardIrqState::new();
        state.activate();
        let stale = state.mask_card().unwrap();
        state.deactivate();
        state.activate();

        assert!(matches!(
            state.rearm_card(stale),
            Err(SdioIrqControlError::StaleGeneration { actual, expected })
                if actual == stale.generation().get() && actual != expected
        ));
    }

    fn read_signal(base: usize) -> u16 {
        // SAFETY: `base` points into the live, aligned fake register array.
        unsafe { core::ptr::read_volatile((base + NORMAL_INT_SIGNAL_ENABLE) as *const u16) }
    }

    fn write_signal(base: usize, value: u16) {
        // SAFETY: `base` points into the live, aligned fake register array.
        unsafe {
            core::ptr::write_volatile((base + NORMAL_INT_SIGNAL_ENABLE) as *mut u16, value);
        }
    }
}
