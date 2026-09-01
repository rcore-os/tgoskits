use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use ringbuf::{HeapCons, HeapProd, HeapRb, traits::Split};
use sdmmc_protocol::sdio::{HostEvent, HostEventKind};

use crate::{ControlRequest, IrqSnapshot, SdioFailure, rdif::owner::AicOwner};

const IRQ_CARD: u8 = 1 << 0;
const IRQ_TRANSFER: u8 = 1 << 1;
const IRQ_ERROR: u8 = 1 << 2;
const IRQ_FLAG_BITS: u32 = 3;
const IRQ_FLAG_MASK: u64 = (1 << IRQ_FLAG_BITS) - 1;
const IRQ_SEQUENCE_MASK: u64 = u64::MAX >> IRQ_FLAG_BITS;

/// Preallocated hard-IRQ to owner snapshot latch.
pub(crate) struct IrqLatch {
    state: AtomicU64,
}

impl IrqLatch {
    pub(crate) const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
        }
    }

    pub(crate) fn publish(&self, event: &impl HostEvent) -> bool {
        let mut flags = 0;
        if event.card_interrupt() {
            flags |= IRQ_CARD;
        }
        match event.kind() {
            HostEventKind::None => {}
            HostEventKind::TransferComplete
            | HostEventKind::CommandComplete
            | HostEventKind::ReceiveReady
            | HostEventKind::TransmitReady
            | HostEventKind::Other => flags |= IRQ_TRANSFER,
            HostEventKind::CardInterrupt => flags |= IRQ_CARD,
            HostEventKind::Error => flags |= IRQ_ERROR,
            _ => flags |= IRQ_TRANSFER,
        }
        if flags == 0 {
            return false;
        }
        self.publish_flags(flags);
        true
    }

    pub(crate) fn publish_card_pending(&self) {
        self.publish_flags(IRQ_CARD);
    }

    pub(crate) fn publish_completion_pending(&self) {
        self.publish_flags(IRQ_TRANSFER);
    }

    fn publish_flags(&self, flags: u8) {
        let mut current = self.state.load(Ordering::Relaxed);
        loop {
            let sequence = ((current >> IRQ_FLAG_BITS).wrapping_add(1)) & IRQ_SEQUENCE_MASK;
            let next = (sequence << IRQ_FLAG_BITS) | ((current & IRQ_FLAG_MASK) | u64::from(flags));
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn take(&self) -> Option<IrqSnapshot> {
        self.take_with_before_clear(|| {})
    }

    fn take_with_before_clear(&self, before_clear: impl FnOnce()) -> Option<IrqSnapshot> {
        let mut current = self.state.load(Ordering::Acquire);
        let mut before_clear = Some(before_clear);
        loop {
            let flags = (current & IRQ_FLAG_MASK) as u8;
            if flags == 0 {
                return None;
            }
            if let Some(before_clear) = before_clear.take() {
                before_clear();
            }
            match self.state.compare_exchange_weak(
                current,
                current & !IRQ_FLAG_MASK,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(IrqSnapshot {
                        sequence: current >> IRQ_FLAG_BITS,
                        card_interrupt: flags & IRQ_CARD != 0,
                        transfer_complete: flags & IRQ_TRANSFER != 0,
                        error: (flags & IRQ_ERROR != 0).then_some(SdioFailure::Bus),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.state.load(Ordering::Acquire) & IRQ_FLAG_MASK != 0
    }

    pub(crate) fn diagnostic(&self) -> (u64, bool) {
        let state = self.state.load(Ordering::Acquire);
        (state >> IRQ_FLAG_BITS, state & IRQ_FLAG_MASK != 0)
    }
}

/// Lock-free MAC publication from owner startup to the general control port.
pub(crate) struct MacAddressState(AtomicU64);

impl MacAddressState {
    pub(crate) fn new(address: [u8; 6]) -> Self {
        Self(AtomicU64::new(encode_mac(address)))
    }

    pub(crate) fn publish(&self, address: [u8; 6]) {
        self.0.store(encode_mac(address), Ordering::Release);
    }

    pub(crate) fn load(&self) -> [u8; 6] {
        decode_mac(self.0.load(Ordering::Acquire))
    }
}

fn encode_mac(address: [u8; 6]) -> u64 {
    let mut raw = [0; 8];
    raw[..6].copy_from_slice(&address);
    u64::from_le_bytes(raw)
}

fn decode_mac(value: u64) -> [u8; 6] {
    let raw = value.to_le_bytes();
    [raw[0], raw[1], raw[2], raw[3], raw[4], raw[5]]
}

pub(crate) type OwnerSender<H> = HeapProd<AicOwner<H>>;
pub(crate) type OwnerReceiver<H> = HeapCons<AicOwner<H>>;

pub(crate) struct OwnerChannels<H: sdmmc_protocol::sdio::SdMmcIrqHost + 'static> {
    pub(crate) sender: OwnerSender<H>,
    pub(crate) receiver: OwnerReceiver<H>,
}

impl<H: sdmmc_protocol::sdio::SdMmcIrqHost + 'static> OwnerChannels<H> {
    pub(crate) fn new() -> Self {
        let ring = HeapRb::new(1);
        let (sender, receiver) = ring.split();
        Self { sender, receiver }
    }
}

pub(crate) type WifiRequestSender = HeapProd<ControlRequest>;
pub(crate) type WifiRequestReceiver = HeapCons<ControlRequest>;
pub(crate) type WifiProgressSender =
    HeapProd<Result<rdif_eth::WifiControlProgress, crate::AicError>>;
pub(crate) type WifiProgressReceiver =
    HeapCons<Result<rdif_eth::WifiControlProgress, crate::AicError>>;

/// Monotonic accounting for progress items crossing the owner/control split.
///
/// The owner can publish a completion while the control endpoint is waiting
/// for an SDIO IRQ.  Counting published and consumed items lets the poll
/// endpoint report that work is ready even when no new hardware IRQ arrives,
/// without making the bounded progress ring unbounded or coupling the owner
/// to the queue-runtime wake primitive.
pub(crate) struct WifiProgressSignal {
    published: AtomicU64,
    consumed: AtomicU64,
}

impl WifiProgressSignal {
    pub(crate) fn new() -> Self {
        Self {
            published: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
        }
    }

    pub(crate) fn publish(&self) {
        self.published.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn consume(&self) {
        self.consumed.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.published.load(Ordering::Acquire) != self.consumed.load(Ordering::Acquire)
    }
}

pub(crate) struct WifiChannels {
    pub(crate) requests_tx: WifiRequestSender,
    pub(crate) requests_rx: WifiRequestReceiver,
    pub(crate) progress_tx: WifiProgressSender,
    pub(crate) progress_rx: WifiProgressReceiver,
    pub(crate) progress_signal: Arc<WifiProgressSignal>,
}

impl WifiChannels {
    pub(crate) fn new() -> Self {
        let requests = HeapRb::new(2);
        let progress = HeapRb::new(8);
        let (requests_tx, requests_rx) = requests.split();
        let (progress_tx, progress_rx) = progress.split();
        Self {
            requests_tx,
            requests_rx,
            progress_tx,
            progress_rx,
            progress_signal: Arc::new(WifiProgressSignal::new()),
        }
    }
}

pub(crate) fn shared_irq_latch() -> Arc<IrqLatch> {
    Arc::new(IrqLatch::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestEvent(HostEventKind);

    impl HostEvent for TestEvent {
        fn kind(&self) -> HostEventKind {
            self.0
        }
    }

    #[test]
    fn irq_published_while_snapshot_is_taken_is_never_hidden_by_the_same_sequence() {
        let latch = IrqLatch::new();
        assert!(latch.publish(&TestEvent(HostEventKind::TransferComplete)));

        let first = latch
            .take_with_before_clear(|| {
                assert!(latch.publish(&TestEvent(HostEventKind::CardInterrupt)));
            })
            .unwrap();
        let second = latch.take();

        let coalesced = first.transfer_complete && first.card_interrupt;
        let separately_ordered = second
            .is_some_and(|snapshot| snapshot.card_interrupt && snapshot.sequence > first.sequence);
        assert!(coalesced || separately_ordered);
    }

    #[test]
    fn mac_publication_round_trips_all_six_bytes() {
        let state = MacAddressState::new([2, 1, 2, 3, 4, 5]);
        assert_eq!(state.load(), [2, 1, 2, 3, 4, 5]);
        state.publish([6, 7, 8, 9, 10, 11]);
        assert_eq!(state.load(), [6, 7, 8, 9, 10, 11]);
    }

    #[test]
    fn progress_signal_tracks_each_item_until_control_consumes_it() {
        let signal = WifiProgressSignal::new();
        assert!(!signal.has_pending());
        signal.publish();
        signal.publish();
        assert!(signal.has_pending());
        signal.consume();
        assert!(signal.has_pending());
        signal.consume();
        assert!(!signal.has_pending());
    }
}
