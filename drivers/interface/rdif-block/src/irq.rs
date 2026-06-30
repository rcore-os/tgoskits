use alloc::vec::Vec;

use crate::{RequestId, RequestToken};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqSourceInfo {
    pub id: usize,
    pub queues: IdList,
}

impl IrqSourceInfo {
    pub const fn new(id: usize, queues: IdList) -> Self {
        Self { id, queues }
    }

    pub const fn legacy(queues: IdList) -> Self {
        Self { id: 0, queues }
    }
}

pub type IrqSourceList = Vec<IrqSourceInfo>;

pub trait IrqHandler: Send + 'static {
    /// Handle a device interrupt in hard IRQ context.
    ///
    /// Implementations must acknowledge or clear the device-side interrupt
    /// source before returning. The returned event is a stable hint for the OS
    /// runtime; task context is still responsible for consuming completions and
    /// completing block requests.
    ///
    /// Hard IRQ handlers must not call OS task, wake, or filesystem APIs, must
    /// not copy DMA buffers for completed requests, and must not update an OS
    /// block runtime pending table. Drivers that need to consume device queue
    /// state to clear the interrupt should cache those completions internally
    /// and return a queue-level event.
    fn handle_irq(&mut self) -> Event;
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdList(u64);

impl IdList {
    pub const fn none() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub fn contains(&self, id: usize) -> bool {
        id < 64 && (self.0 & (1 << id)) != 0
    }

    pub fn insert(&mut self, id: usize) {
        if id < 64 {
            self.0 |= 1 << id;
        }
    }

    pub fn remove(&mut self, id: usize) {
        if id < 64 {
            self.0 &= !(1 << id);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> {
        (0..64).filter(move |i| self.contains(*i))
    }
}

pub const MAX_COMPLETION_HINTS: usize = 8;
pub const MAX_BATCH_COMPLETION_IDS: usize = 16;

#[derive(Debug, Clone, Copy)]
pub enum CompletionHint {
    Queue {
        queue_id: usize,
    },
    Request {
        queue_id: usize,
        request_id: RequestId,
    },
    Token {
        queue_id: usize,
        token: RequestToken,
    },
    Batch {
        queue_id: usize,
        ids: CompletionIds,
    },
}

impl CompletionHint {
    pub const fn queue_id(self) -> usize {
        match self {
            Self::Queue { queue_id }
            | Self::Request { queue_id, .. }
            | Self::Token { queue_id, .. }
            | Self::Batch { queue_id, .. } => queue_id,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CompletionIds {
    len: usize,
    ids: [RequestId; MAX_BATCH_COMPLETION_IDS],
}

impl CompletionIds {
    pub const fn new() -> Self {
        Self {
            len: 0,
            ids: [RequestId::new(0); MAX_BATCH_COMPLETION_IDS],
        }
    }

    pub fn push(&mut self, request_id: RequestId) -> bool {
        if self.len == self.ids.len() {
            return false;
        }
        self.ids[self.len] = request_id;
        self.len += 1;
        true
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = RequestId> + '_ {
        self.ids[..self.len].iter().copied()
    }
}

impl Default for CompletionIds {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CompletionList {
    len: usize,
    hints: [Option<CompletionHint>; MAX_COMPLETION_HINTS],
}

impl CompletionList {
    pub const fn new() -> Self {
        Self {
            len: 0,
            hints: [None; MAX_COMPLETION_HINTS],
        }
    }

    pub fn push(&mut self, hint: CompletionHint) -> bool {
        if self.len == self.hints.len() {
            return false;
        }
        self.hints[self.len] = Some(hint);
        self.len += 1;
        true
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = CompletionHint> + '_ {
        self.hints[..self.len]
            .iter()
            .filter_map(|hint| hint.as_ref().copied())
    }
}

impl Default for CompletionList {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub queues: IdList,
    pub completions: CompletionList,
}

impl Event {
    pub const fn none() -> Self {
        Self {
            queues: IdList::none(),
            completions: CompletionList::new(),
        }
    }

    pub const fn from_queue_bits(bits: u64) -> Self {
        Self {
            queues: IdList::from_bits(bits),
            completions: CompletionList::new(),
        }
    }

    pub fn from_hint(hint: CompletionHint) -> Self {
        let mut event = Self::none();
        event.push_hint(hint);
        event
    }

    pub fn push_queue(&mut self, queue_id: usize) {
        self.queues.insert(queue_id);
        let _ = self.completions.push(CompletionHint::Queue { queue_id });
    }

    pub fn push_request(&mut self, queue_id: usize, request_id: RequestId) {
        if !self.completions.push(CompletionHint::Request {
            queue_id,
            request_id,
        }) {
            self.queues.insert(queue_id);
        }
    }

    pub fn push_token(&mut self, queue_id: usize, token: RequestToken) {
        if !self
            .completions
            .push(CompletionHint::Token { queue_id, token })
        {
            self.queues.insert(queue_id);
        }
    }

    pub fn push_hint(&mut self, hint: CompletionHint) {
        if let CompletionHint::Queue { queue_id } = hint {
            self.queues.insert(queue_id);
        }
        if !self.completions.push(hint) {
            self.queues.insert(hint.queue_id());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.queues.bits() == 0 && self.completions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irq_source_lists_queue_masks() {
        let mut queues = IdList::none();
        queues.insert(2);
        let source = IrqSourceInfo::legacy(queues);

        assert_eq!(source.id, 0);
        assert!(source.queues.contains(2));
    }

    #[test]
    fn completion_hints_preserve_queue_level_compatibility() {
        let mut event = Event::none();
        event.push_queue(3);
        event.push_request(3, RequestId::new(7));
        event.push_token(
            3,
            crate::RequestToken::new(RequestId::new(8), crate::RequestGeneration::new(2)),
        );

        assert!(event.queues.contains(3));
        assert_eq!(event.completions.len(), 3);
    }

    #[test]
    fn completion_hint_overflow_falls_back_to_queue_bit() {
        let mut event = Event::none();
        for id in 0..(MAX_COMPLETION_HINTS + 1) {
            event.push_request(5, RequestId::new(id));
        }

        assert_eq!(event.completions.len(), MAX_COMPLETION_HINTS);
        assert!(event.queues.contains(5));
    }

    #[test]
    fn queue_event_represents_driver_local_completion_ready() {
        let event = Event::from_hint(CompletionHint::Queue { queue_id: 2 });

        assert!(event.queues.contains(2));
        assert_eq!(event.completions.len(), 1);
    }
}
