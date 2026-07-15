//! Bounded intrusive SMP scheduler inboxes.
//!
//! Producers publish embedded nodes using one lock-free compare/exchange loop.
//! The owner drains detached FIFO snapshots into caller-provided storage.

mod message;
mod node;

use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

pub use message::{InboxKind, InboxMessage};
pub use node::InboxNode;

use crate::epoch_mpsc::EpochMpscQueue;

/// Result of publishing one embedded scheduler node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishResult {
    /// The node was published and now owns one inbox membership.
    Published,
    /// This node already represents a coalesced pending request.
    AlreadyPending,
    /// The node or message belongs to another inbox class.
    WrongKind,
}

/// Result of one bounded owner-side drain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DrainBatch {
    drained: usize,
    pending: bool,
}

impl DrainBatch {
    /// Returns messages written to caller storage.
    pub const fn drained(self) -> usize {
        self.drained
    }

    /// Reports whether another drain is required.
    pub const fn pending(self) -> bool {
        self.pending
    }
}

/// Lock-free intrusive inbox for one scheduler message class.
#[derive(Debug)]
pub struct SchedulerInbox {
    kind: InboxKind,
    publication: EpochMpscQueue<InboxNode>,
    pending: AtomicPtr<InboxNode>,
    draining: AtomicBool,
}

impl SchedulerInbox {
    /// Creates an empty remote-wake, migration, or reclaim inbox.
    pub const fn new(kind: InboxKind) -> Self {
        Self {
            kind,
            publication: EpochMpscQueue::new(),
            pending: AtomicPtr::new(ptr::null_mut()),
            draining: AtomicBool::new(false),
        }
    }

    /// Coalesces and publishes one embedded request without allocating.
    pub fn publish(
        &self,
        node: core::pin::Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        self.publish_with_head_transition(node, message).0
    }

    /// Publishes and reports an empty-head to non-empty-head transition.
    ///
    /// The transition is the producer-side scheduler-IPI epoch, analogous to
    /// Linux `llist_add()` returning whether the lock-free list was empty.
    pub(crate) fn publish_with_head_transition(
        &self,
        node: core::pin::Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> (PublishResult, bool) {
        if node.kind() != self.kind || message.kind() != self.kind {
            return (PublishResult::WrongKind, false);
        }
        if !node.reserve(message) {
            return (PublishResult::AlreadyPending, false);
        }

        let node = node.get_ref() as *const InboxNode as *mut InboxNode;
        let transitioned = unsafe {
            // Reservation owns this pinned node and its scheduler-inbox link
            // until the epoch-graced publication transfers that membership.
            self.publication.publish(node, (*node).next())
        };
        (PublishResult::Published, transitioned)
    }

    /// Drains at most `limit` messages into preallocated caller storage.
    ///
    /// Concurrent drain attempts return immediately with `pending = true`; the
    /// owner CPU remains the only logical consumer.
    pub fn drain(&self, limit: usize, output: &mut [InboxMessage]) -> DrainBatch {
        if self
            .draining
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return DrainBatch {
                drained: 0,
                pending: true,
            };
        }

        let mut cursor = self.take_snapshot();
        let bound = limit.min(output.len());
        let mut drained = 0;
        while !cursor.is_null() && drained < bound {
            let node = cursor;
            cursor = unsafe {
                // The drain guard gives this consumer exclusive ownership of the
                // detached list and its intrusive links.
                (*node).take_next()
            };
            output[drained] = unsafe {
                // Acquire detachment observes the payload written before Release
                // publication, and no producer may reuse it while queued is set.
                (*node).take_message()
            };
            drained += 1;
        }

        self.pending.store(cursor, Ordering::Release);
        let pending = !cursor.is_null() || !self.publication.is_empty();
        self.draining.store(false, Ordering::Release);
        DrainBatch { drained, pending }
    }

    /// Reports whether producer or partially drained work remains.
    pub fn has_pending(&self) -> bool {
        !self.pending.load(Ordering::Acquire).is_null() || !self.publication.is_empty()
    }

    fn take_snapshot(&self) -> *mut InboxNode {
        let pending = self.pending.swap(ptr::null_mut(), Ordering::Acquire);
        if !pending.is_null() {
            return pending;
        }
        let stack = unsafe {
            // `draining` makes this the only consumer. A null result may mean
            // the old publication epoch is still crossing its grace period.
            self.publication.take_graced_stack()
        };
        unsafe {
            // The drain guard and completed epoch grace make this consumer the
            // sole owner of the detached stack and every observed provenance.
            reverse(stack)
        }
    }

    #[cfg(test)]
    fn arm_test_publisher_pause(&self) {
        self.publication.arm_test_publisher_pause();
    }

    #[cfg(test)]
    fn wait_for_test_publisher_pause(&self) {
        self.publication.wait_for_test_publisher_pause();
    }

    #[cfg(test)]
    fn resume_test_publisher(&self) {
        self.publication.resume_test_publisher();
    }

    #[cfg(test)]
    fn arm_test_generation_pause(&self) {
        self.publication.arm_test_generation_pause();
    }

    #[cfg(test)]
    fn wait_for_test_generation_pause(&self) {
        self.publication.wait_for_test_generation_pause();
    }

    #[cfg(test)]
    fn resume_test_generation_publisher(&self) {
        self.publication.resume_test_generation_publisher();
    }
}

/// Reverses one exclusively detached producer stack into FIFO order.
///
/// # Safety
///
/// `cursor` must be a detached list of pinned nodes whose queue memberships keep
/// them alive, and the caller must be its exclusive consumer.
unsafe fn reverse(mut cursor: *mut InboxNode) -> *mut InboxNode {
    let mut reversed = ptr::null_mut();
    while !cursor.is_null() {
        let next = unsafe {
            // Every link in the detached list is exclusively consumer-owned.
            (*cursor).next().load(Ordering::Relaxed)
        };
        unsafe {
            // The node cannot be republished until `take_message` clears queued.
            (*cursor).next().store(reversed, Ordering::Relaxed);
        }
        reversed = cursor;
        cursor = next;
    }
    reversed
}

#[cfg(test)]
mod tests;
