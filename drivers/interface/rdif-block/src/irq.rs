use alloc::vec::Vec;
use core::num::NonZeroU64;

use crate::{BlkError, RequestId, ServiceContinuation, ServiceContinuationReason, ServiceProgress};

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

/// How far one shared block-controller IRQ action progressed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IrqDisposition {
    Unhandled,
    Acknowledged,
    Deferred,
}

/// Explicit result of one shared block-controller IRQ action.
#[derive(Clone, Copy, Debug)]
pub struct IrqOutcome {
    disposition: IrqDisposition,
    event: Event,
}

impl IrqOutcome {
    pub const fn unhandled() -> Self {
        Self {
            disposition: IrqDisposition::Unhandled,
            event: Event::none(),
        }
    }

    /// Reports an acknowledged controller event with no queue completion.
    pub const fn handled_control() -> Self {
        Self {
            disposition: IrqDisposition::Acknowledged,
            event: Event::none(),
        }
    }

    /// Reports immutable facts produced after destructive acknowledgement.
    pub const fn handled(event: Event) -> Self {
        Self {
            disposition: IrqDisposition::Acknowledged,
            event,
        }
    }

    /// Requests one source-owned acknowledgement continuation.
    ///
    /// `queues` is only a routing mask. It never transfers MMIO/W1C ownership
    /// to any queue and is not an acknowledged completion event.
    pub const fn deferred(queues: IdList) -> Self {
        Self {
            disposition: IrqDisposition::Deferred,
            event: Event::from_queue_bits(queues.bits()),
        }
    }

    /// Classifies a prebuilt event without allowing split acknowledgement
    /// state: empty events are unhandled and all other facts are acknowledged.
    pub const fn from_event(event: Event) -> Self {
        if event.is_empty() {
            Self::unhandled()
        } else {
            Self::handled(event)
        }
    }

    pub const fn is_handled(self) -> bool {
        !matches!(self.disposition, IrqDisposition::Unhandled)
    }

    pub const fn is_deferred(self) -> bool {
        matches!(self.disposition, IrqDisposition::Deferred)
    }

    /// Returns immutable queue facts only after the source was acknowledged.
    pub const fn acknowledged_event(self) -> Option<Event> {
        if matches!(self.disposition, IrqDisposition::Acknowledged) {
            Some(self.event)
        } else {
            None
        }
    }

    /// Returns routing hints for one source-owned deferred continuation.
    pub const fn deferred_queues(self) -> Option<IdList> {
        if matches!(self.disposition, IrqDisposition::Deferred) {
            Some(self.event.queues)
        } else {
            None
        }
    }
}

/// Result of continuing one source-owned destructive acknowledgement.
#[derive(Debug, Clone, Copy)]
pub enum DeferredIrqProgress {
    /// The device no longer reports this source; the line may reopen.
    Unhandled,
    /// Destructive acknowledgement completed and produced immutable facts.
    Acknowledged(Event),
    /// Register or transport ownership is still contended; retain the token.
    Deferred,
    /// The source cannot be acknowledged and controller recovery is required.
    Failed(BlkError),
}

pub trait IrqHandler: Send + 'static {
    /// Handle a device interrupt in hard IRQ context.
    ///
    /// Normally the top half acknowledges or clears the device-side source
    /// before returning and publishes a stable event snapshot. A transport may
    /// instead return [`IrqOutcome::deferred`] when destructive acknowledgement
    /// must be serialized with task context. The runtime then invokes
    /// [`Self::continue_deferred_irq`] on this same per-source endpoint while
    /// its backing line remains masked. Queues never own this operation.
    ///
    /// Hard IRQ handlers must not call OS task, wake, or filesystem APIs, must
    /// not copy DMA buffers for completed requests, and must not update an OS
    /// block runtime pending table. Drivers that need to consume device queue
    /// state to clear the interrupt should cache those completions internally
    /// and return a queue-level event.
    fn handle_irq(&mut self) -> IrqOutcome;

    /// Continues a previously deferred acknowledgement in bounded task context.
    ///
    /// The default fails closed so a handler cannot publish `Deferred` without
    /// also implementing the unique continuation owner.
    fn continue_deferred_irq(&mut self) -> DeferredIrqProgress {
        DeferredIrqProgress::Failed(BlkError::NotSupported)
    }
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

    pub const fn is_empty(self) -> bool {
        self.0 == 0
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

/// Monotonic generation of acknowledged facts from one IRQ source endpoint.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IrqEventEpoch(NonZeroU64);

impl IrqEventEpoch {
    pub const INITIAL: Self = Self(NonZeroU64::MIN);

    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Copyable queue facts produced by exactly one acknowledged IRQ source.
#[derive(Debug, Clone, Copy)]
pub struct AcknowledgedEvent {
    source_id: usize,
    epoch: IrqEventEpoch,
    facts: Event,
}

impl AcknowledgedEvent {
    pub const fn new(source_id: usize, epoch: IrqEventEpoch, facts: Event) -> Self {
        Self {
            source_id,
            epoch,
            facts,
        }
    }

    pub const fn source_id(self) -> usize {
        self.source_id
    }

    pub const fn epoch(self) -> IrqEventEpoch {
        self.epoch
    }

    pub const fn facts(self) -> Event {
        self.facts
    }

    pub fn for_queue(&self, queue_id: usize) -> Option<QueueEventBatch<'_>> {
        self.facts
            .for_queue_with_source(queue_id, self.source_id, self.epoch)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub queues: IdList,
    pub completions: CompletionList,
}

/// Queue-local view of one device interrupt event.
///
/// This value can only be obtained from [`Event::for_queue`]. It preserves the
/// boundary between the source acknowledgement endpoint and queue service.
/// Every event represented here is already acknowledged and freely copyable.
#[derive(Debug, Clone, Copy)]
pub struct QueueEventBatch<'event> {
    queue_id: usize,
    source_id: usize,
    source_epoch: IrqEventEpoch,
    event: &'event Event,
}

impl QueueEventBatch<'_> {
    pub const fn queue_id(&self) -> usize {
        self.queue_id
    }

    pub const fn source_id(&self) -> usize {
        self.source_id
    }

    pub const fn source_epoch(&self) -> IrqEventEpoch {
        self.source_epoch
    }

    pub fn hints(&self) -> impl Iterator<Item = CompletionHint> + '_ {
        self.event
            .completions
            .iter()
            .filter(move |hint| hint.queue_id() == self.queue_id)
    }

    pub fn has_queue_signal(&self) -> bool {
        self.event.queues.contains(self.queue_id)
            || self
                .hints()
                .any(|hint| matches!(hint, CompletionHint::Queue { .. }))
    }

    /// Retains this exact acknowledged source epoch for one bounded rerun.
    pub const fn continue_service(
        &self,
        reason: ServiceContinuationReason,
    ) -> ServiceProgress {
        ServiceProgress::Continue(ServiceContinuation::new(
            self.source_id,
            self.source_epoch,
            reason,
        ))
    }

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

    pub fn push_hint(&mut self, hint: CompletionHint) {
        if let CompletionHint::Queue { queue_id } = hint {
            self.queues.insert(queue_id);
        }
        if !self.completions.push(hint) {
            self.queues.insert(hint.queue_id());
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.queues.bits() == 0 && self.completions.is_empty()
    }

    /// Borrows the stable hints for one queue affected by this IRQ event.
    pub fn for_queue(&self, queue_id: usize) -> Option<QueueEventBatch<'_>> {
        self.for_queue_with_source(queue_id, 0, IrqEventEpoch::INITIAL)
    }

    fn for_queue_with_source(
        &self,
        queue_id: usize,
        source_id: usize,
        source_epoch: IrqEventEpoch,
    ) -> Option<QueueEventBatch<'_>> {
        let affected = self.queues.contains(queue_id)
            || self
                .completions
                .iter()
                .any(|hint| hint.queue_id() == queue_id);
        affected.then_some(QueueEventBatch {
            queue_id,
            source_id,
            source_epoch,
            event: self,
        })
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

        assert!(event.queues.contains(3));
        assert_eq!(event.completions.len(), 2);
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

    #[test]
    fn queue_event_batch_filters_an_acknowledged_irq_event() {
        let mut event = Event::none();
        event.push_request(1, RequestId::new(3));
        event.push_request(2, RequestId::new(4));
        event.push_queue(2);

        let batch = event.for_queue(2).expect("queue 2 must be affected");

        assert_eq!(batch.queue_id(), 2);
        assert!(batch.has_queue_signal());
        assert_eq!(batch.hints().count(), 2);
        assert!(event.for_queue(7).is_none());
    }

    #[test]
    fn one_irq_source_never_transfers_ack_ownership_to_multiple_queues() {
        let queues = IdList::from_bits((1 << 2) | (1 << 5));
        let outcome = IrqOutcome::deferred(queues);

        assert!(outcome.acknowledged_event().is_none());
        assert_eq!(outcome.deferred_queues(), Some(queues));
    }

    #[test]
    fn acknowledged_facts_preserve_source_identity_across_queue_fanout() {
        let facts = Event::from_queue_bits((1 << 2) | (1 << 5));
        let event = AcknowledgedEvent::new(7, IrqEventEpoch::new(11).unwrap(), facts);

        let first = event.for_queue(2).unwrap();
        let second = event.for_queue(5).unwrap();
        assert_eq!((first.source_id(), first.source_epoch().get()), (7, 11));
        assert_eq!((second.source_id(), second.source_epoch().get()), (7, 11));
    }

    #[test]
    fn two_sources_routed_to_one_queue_remain_distinct() {
        let facts = Event::from_queue_bits(1 << 3);
        let source_a = AcknowledgedEvent::new(1, IrqEventEpoch::new(4).unwrap(), facts);
        let source_b = AcknowledgedEvent::new(2, IrqEventEpoch::new(9).unwrap(), facts);

        let source_a = source_a.for_queue(3).unwrap();
        let source_b = source_b.for_queue(3).unwrap();
        assert_ne!(source_a.source_id(), source_b.source_id());
        assert_ne!(source_a.source_epoch(), source_b.source_epoch());
    }
}
