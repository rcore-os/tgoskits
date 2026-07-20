use alloc::vec::Vec;
use core::num::NonZeroU64;

use crate::{BlkError, ServiceProgress, ServiceRerun, ServiceRerunReason};

/// Stable result of one block-device interrupt endpoint invocation.
pub type BlockIrqCapture = rdif_irq::IrqCapture<Event, BlkError>;

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

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event(IdList);

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

    /// Returns every queue affected by the same acknowledged source event.
    pub const fn affected_queues(&self) -> IdList {
        self.event.queues()
    }

    /// Requests another bounded work pass for this captured source epoch.
    pub const fn requeue_service(&self, reason: ServiceRerunReason) -> ServiceProgress {
        ServiceProgress::Requeue(ServiceRerun::new(self.source_id, self.source_epoch, reason))
    }
}

impl Event {
    pub const fn none() -> Self {
        Self(IdList::none())
    }

    pub const fn from_queue_bits(bits: u64) -> Self {
        Self(IdList::from_bits(bits))
    }

    pub fn push_queue(&mut self, queue_id: usize) {
        self.0.insert(queue_id);
    }

    pub const fn queues(&self) -> IdList {
        self.0
    }

    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Borrows proof that one queue is affected by this IRQ event.
    pub fn for_queue(&self, queue_id: usize) -> Option<QueueEventBatch<'_>> {
        self.for_queue_with_source(queue_id, 0, IrqEventEpoch::INITIAL)
    }

    fn for_queue_with_source(
        &self,
        queue_id: usize,
        source_id: usize,
        source_epoch: IrqEventEpoch,
    ) -> Option<QueueEventBatch<'_>> {
        self.0.contains(queue_id).then_some(QueueEventBatch {
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
    fn queue_event_accumulates_affected_queue_bits() {
        let mut event = Event::none();
        event.push_queue(3);
        event.push_queue(5);

        assert_eq!(event.queues().bits(), (1 << 3) | (1 << 5));
    }

    #[test]
    fn queue_event_represents_driver_local_completion_ready() {
        let event = Event::from_queue_bits(1 << 2);

        assert!(event.queues().contains(2));
    }

    #[test]
    fn queue_event_batch_filters_an_acknowledged_irq_event() {
        let event = Event::from_queue_bits((1 << 1) | (1 << 2));

        let batch = event.for_queue(2).expect("queue 2 must be affected");

        assert_eq!(batch.queue_id(), 2);
        assert_eq!(batch.affected_queues(), event.queues());
        assert!(event.for_queue(7).is_none());
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
