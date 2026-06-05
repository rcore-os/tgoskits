use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use rdif_block::{CompletionHint, CompletionList, Event};

const IRQ_HINT_SLOTS: usize = rdif_block::MAX_COMPLETION_HINTS;

pub struct BlockIrqBridge {
    queue_bits: AtomicU64,
    hint_slots: [AtomicHintSlot; IRQ_HINT_SLOTS],
    drain_ready: AtomicBool,
}

#[derive(Clone, Copy, Debug)]
pub struct DrainEvents {
    pub queue_bits: u64,
    pub hints: CompletionList,
}

impl BlockIrqBridge {
    pub const fn new() -> Self {
        Self {
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
                    self.record_queue_ready(hint.queue_id());
                }
            }
        }

        self.drain_ready.store(true, Ordering::Release);
    }

    pub fn record_hint(&self, hint: CompletionHint) {
        if !self.push_hint_slot(hint) {
            self.record_queue_ready(hint.queue_id());
        }
        self.drain_ready.store(true, Ordering::Release);
    }

    pub fn record_queue_ready(&self, queue_id: usize) {
        if queue_id < u64::BITS as usize {
            self.queue_bits.fetch_or(1 << queue_id, Ordering::AcqRel);
        }
        self.drain_ready.store(true, Ordering::Release);
    }

    pub fn drain_ready(&self) -> bool {
        self.drain_ready.load(Ordering::Acquire)
    }

    pub fn take_events(&self) -> DrainEvents {
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
        DrainEvents { queue_bits, hints }
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
