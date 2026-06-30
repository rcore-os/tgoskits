use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use rdif_block::{CompletionHint, CompletionList, Event};

const IRQ_HINT_SLOTS: usize = rdif_block::MAX_COMPLETION_HINTS;

pub struct BlockIrqBridge {
    seq: AtomicU64,
    queue_bits: AtomicU64,
    hint_slots: [AtomicHintSlot; IRQ_HINT_SLOTS],
    drain_ready: AtomicBool,
}

pub struct RuntimeEventLatch {
    bridge: Arc<BlockIrqBridge>,
    driver_queue_map: Vec<Option<usize>>,
}

#[derive(Clone, Copy, Debug)]
pub struct DrainEvents {
    pub seq: u64,
    pub queue_bits: u64,
    pub hints: CompletionList,
}

impl RuntimeEventLatch {
    pub fn new(
        bridge: Arc<BlockIrqBridge>,
        driver_queue_map: impl IntoIterator<Item = Option<usize>>,
    ) -> Self {
        Self {
            bridge,
            driver_queue_map: driver_queue_map.into_iter().collect(),
        }
    }

    pub fn bridge(&self) -> Arc<BlockIrqBridge> {
        self.bridge.clone()
    }

    pub fn record_driver_event(&self, event: Event) -> bool {
        let mut translated = Event::none();
        for driver_queue_id in event.queues.iter() {
            if let Some(runtime_queue_id) = self.runtime_queue_id(driver_queue_id) {
                translated.queues.insert(runtime_queue_id);
            }
        }
        for hint in event.completions.iter() {
            if let Some(hint) = self.translate_driver_hint(hint) {
                translated.push_hint(hint);
            }
        }
        if translated.is_empty() {
            return false;
        }
        self.bridge.record_event(translated);
        true
    }

    fn runtime_queue_id(&self, driver_queue_id: usize) -> Option<usize> {
        self.driver_queue_map
            .get(driver_queue_id)
            .copied()
            .flatten()
    }

    fn translate_driver_hint(&self, hint: CompletionHint) -> Option<CompletionHint> {
        let queue_id = self.runtime_queue_id(hint.queue_id())?;
        Some(match hint {
            CompletionHint::Queue { .. } => CompletionHint::Queue { queue_id },
            CompletionHint::Request { request_id, .. } => CompletionHint::Request {
                queue_id,
                request_id,
            },
            CompletionHint::Token { token, .. } => CompletionHint::Token { queue_id, token },
            CompletionHint::Batch { ids, .. } => CompletionHint::Batch { queue_id, ids },
        })
    }
}

impl BlockIrqBridge {
    pub const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            queue_bits: AtomicU64::new(0),
            hint_slots: [const { AtomicHintSlot::new() }; IRQ_HINT_SLOTS],
            drain_ready: AtomicBool::new(false),
        }
    }

    pub fn record_event(&self, event: Event) {
        if event.queues.bits() != 0 {
            self.queue_bits
                .fetch_or(event.queues.bits(), Ordering::AcqRel);
        }

        if !event.completions.is_empty() {
            for hint in event.completions.iter() {
                if !self.push_hint_slot(hint) {
                    self.record_queue_ready_inner(hint.queue_id());
                }
            }
        }

        self.publish_seq();
    }

    pub fn record_hint(&self, hint: CompletionHint) {
        if !self.push_hint_slot(hint) {
            self.record_queue_ready_inner(hint.queue_id());
        }
        self.publish_seq();
    }

    pub fn record_queue_ready(&self, queue_id: usize) {
        self.record_queue_ready_inner(queue_id);
        self.publish_seq();
    }

    fn record_queue_ready_inner(&self, queue_id: usize) {
        if queue_id < u64::BITS as usize {
            self.queue_bits.fetch_or(1 << queue_id, Ordering::AcqRel);
        }
    }

    fn publish_seq(&self) {
        self.drain_ready.store(true, Ordering::Release);
        self.seq.fetch_add(1, Ordering::AcqRel);
    }

    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    pub fn has_changed(&self, observed: u64) -> bool {
        self.seq() != observed
    }

    pub fn drain_ready(&self) -> bool {
        self.drain_ready.load(Ordering::Acquire)
    }

    pub fn take_events(&self) -> DrainEvents {
        let seq = self.seq();
        self.drain_ready.store(false, Ordering::Release);
        let queue_bits = self.queue_bits.swap(0, Ordering::AcqRel);
        let mut hints = CompletionList::new();
        for slot in &self.hint_slots {
            if let Some(hint) = slot.take() {
                let _ = hints.push(hint);
            }
        }
        if self.queue_bits.load(Ordering::Acquire) != 0
            || self.hint_slots.iter().any(AtomicHintSlot::is_occupied)
        {
            self.drain_ready.store(true, Ordering::Release);
        }
        DrainEvents {
            seq,
            queue_bits,
            hints,
        }
    }

    fn push_hint_slot(&self, hint: CompletionHint) -> bool {
        for slot in &self.hint_slots {
            if slot.try_store(hint) {
                return true;
            }
        }
        false
    }
}

impl Default for BlockIrqBridge {
    fn default() -> Self {
        Self::new()
    }
}

struct AtomicHintSlot {
    state: AtomicU8,
    kind: AtomicUsize,
    queue_id: AtomicUsize,
    request_id: AtomicUsize,
    request_generation: AtomicU64,
    batch_len: AtomicUsize,
    batch_ids: [AtomicUsize; rdif_block::MAX_BATCH_COMPLETION_IDS],
}

impl AtomicHintSlot {
    const EMPTY: u8 = 0;
    const WRITING: u8 = 1;
    const FULL: u8 = 2;

    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::EMPTY),
            kind: AtomicUsize::new(0),
            queue_id: AtomicUsize::new(0),
            request_id: AtomicUsize::new(0),
            request_generation: AtomicU64::new(0),
            batch_len: AtomicUsize::new(0),
            batch_ids: [const { AtomicUsize::new(0) }; rdif_block::MAX_BATCH_COMPLETION_IDS],
        }
    }

    fn try_store(&self, hint: CompletionHint) -> bool {
        if self
            .state
            .compare_exchange(
                Self::EMPTY,
                Self::WRITING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }

        self.queue_id.store(hint.queue_id(), Ordering::Relaxed);
        match hint {
            CompletionHint::Queue { .. } => {
                self.kind.store(0, Ordering::Relaxed);
                self.batch_len.store(0, Ordering::Relaxed);
            }
            CompletionHint::Request { request_id, .. } => {
                self.kind.store(1, Ordering::Relaxed);
                self.request_id
                    .store(usize::from(request_id), Ordering::Relaxed);
                self.request_generation.store(0, Ordering::Relaxed);
                self.batch_len.store(0, Ordering::Relaxed);
            }
            CompletionHint::Token { token, .. } => {
                self.kind.store(3, Ordering::Relaxed);
                self.request_id
                    .store(usize::from(token.id), Ordering::Relaxed);
                self.request_generation
                    .store(token.generation.get(), Ordering::Relaxed);
                self.batch_len.store(0, Ordering::Relaxed);
            }
            CompletionHint::Batch { ids, .. } => {
                self.kind.store(2, Ordering::Relaxed);
                let len = ids.len();
                for (idx, request_id) in ids.iter().enumerate() {
                    self.batch_ids[idx].store(usize::from(request_id), Ordering::Relaxed);
                }
                self.batch_len.store(len, Ordering::Relaxed);
            }
        }
        self.state.store(Self::FULL, Ordering::Release);
        true
    }

    fn take(&self) -> Option<CompletionHint> {
        if self
            .state
            .compare_exchange(
                Self::FULL,
                Self::WRITING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return None;
        }

        let queue_id = self.queue_id.load(Ordering::Relaxed);
        let hint = match self.kind.load(Ordering::Relaxed) {
            0 => CompletionHint::Queue { queue_id },
            1 => CompletionHint::Request {
                queue_id,
                request_id: rdif_block::RequestId::new(self.request_id.load(Ordering::Relaxed)),
            },
            3 => CompletionHint::Token {
                queue_id,
                token: rdif_block::RequestToken::new(
                    rdif_block::RequestId::new(self.request_id.load(Ordering::Relaxed)),
                    rdif_block::RequestGeneration::new(
                        self.request_generation.load(Ordering::Relaxed),
                    ),
                ),
            },
            2 => {
                let mut ids = rdif_block::CompletionIds::new();
                let len = self.batch_len.load(Ordering::Relaxed);
                for idx in 0..len.min(rdif_block::MAX_BATCH_COMPLETION_IDS) {
                    let _ = ids.push(rdif_block::RequestId::new(
                        self.batch_ids[idx].load(Ordering::Relaxed),
                    ));
                }
                CompletionHint::Batch { queue_id, ids }
            }
            _ => CompletionHint::Queue { queue_id },
        };
        self.state.store(Self::EMPTY, Ordering::Release);
        Some(hint)
    }

    fn is_occupied(&self) -> bool {
        self.state.load(Ordering::Acquire) != Self::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use super::*;

    #[test]
    fn runtime_event_latch_translates_driver_queue_ids() {
        let bridge = Arc::new(BlockIrqBridge::new());
        let latch = RuntimeEventLatch::new(bridge.clone(), [None, Some(0), None, Some(1)]);

        assert!(latch.record_driver_event(Event::from_queue_bits(1 << 3)));

        let events = bridge.take_events();
        assert_eq!(events.queue_bits, 1 << 1);
    }

    #[test]
    fn runtime_event_latch_ignores_unknown_driver_queue_ids() {
        let bridge = Arc::new(BlockIrqBridge::new());
        let latch = RuntimeEventLatch::new(bridge.clone(), [Some(0)]);

        assert!(!latch.record_driver_event(Event::from_queue_bits(1 << 4)));
        assert!(!bridge.drain_ready());
    }

    #[test]
    fn runtime_event_latch_keeps_event_without_pending_table() {
        let bridge = Arc::new(BlockIrqBridge::new());
        let latch = RuntimeEventLatch::new(bridge.clone(), [Some(0)]);

        assert!(latch.record_driver_event(Event::from_queue_bits(1)));

        let events = bridge.take_events();
        assert_eq!(events.queue_bits, 1);
        assert_eq!(events.seq, 1);
    }

    #[test]
    fn block_irq_bridge_sequences_coalesced_events() {
        let bridge = BlockIrqBridge::new();
        assert_eq!(bridge.seq(), 0);
        assert!(!bridge.has_changed(0));

        bridge.record_queue_ready(0);
        bridge.record_queue_ready(1);

        assert!(bridge.has_changed(0));
        assert_eq!(bridge.seq(), 2);

        let events = bridge.take_events();
        assert_eq!(events.seq, 2);
        assert_eq!(events.queue_bits, 0b11);
        assert!(!bridge.drain_ready());
    }

    #[test]
    fn block_irq_bridge_hint_overflow_falls_back_to_queue_scan_once() {
        let bridge = BlockIrqBridge::new();
        let mut event = Event::none();
        for id in 0..(IRQ_HINT_SLOTS + 1) {
            event.push_request(3, rdif_block::RequestId::new(id));
        }

        bridge.record_event(event);

        assert_eq!(bridge.seq(), 1);
        let events = bridge.take_events();
        assert_eq!(events.seq, 1);
        assert_eq!(events.hints.len(), IRQ_HINT_SLOTS);
        assert_eq!(events.queue_bits, 1 << 3);
    }

    #[test]
    fn block_irq_bridge_preserves_request_token_hints() {
        let bridge = BlockIrqBridge::new();
        let token = rdif_block::RequestToken::new(
            rdif_block::RequestId::new(9),
            rdif_block::RequestGeneration::new(42),
        );
        let mut event = Event::none();
        event.push_token(2, token);

        bridge.record_event(event);

        let events = bridge.take_events();
        let hint = events
            .hints
            .iter()
            .next()
            .expect("token hint should be preserved");
        assert!(matches!(
            hint,
            CompletionHint::Token {
                queue_id: 2,
                token: observed,
            } if observed == token
        ));
    }
}
