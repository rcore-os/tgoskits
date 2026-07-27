//! Intrusive multi-producer, single-consumer coroutine inboxes.

use core::{ptr, sync::atomic::Ordering};

use super::CoroutineHeader;
use crate::epoch_mpsc::EpochMpscQueue;

#[derive(Clone, Copy)]
pub(super) enum InboxKind {
    Ready,
}

pub(super) struct IntrusiveInbox {
    publication: EpochMpscQueue<CoroutineHeader>,
    kind: InboxKind,
}

impl IntrusiveInbox {
    pub(super) const fn new(kind: InboxKind) -> Self {
        Self {
            publication: EpochMpscQueue::new(),
            kind,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.publication.is_empty()
    }

    /// Publishes one node into this multi-producer inbox.
    ///
    /// # Safety
    ///
    /// `header` must be pinned, live through its queue reference, and absent from
    /// every list using this inbox's selected intrusive link.
    pub(super) unsafe fn push(&self, header: *mut CoroutineHeader) {
        let next = unsafe {
            // Caller guarantees exclusive membership for this node and inbox.
            (*header).next(self.kind)
        };
        unsafe {
            // RUN_QUEUED owns this pinned node and its ready link until the
            // epoch-graced publication transfers that queue membership.
            self.publication.publish(header, next);
        }
    }

    /// Detaches and FIFO-orders every node currently visible to the consumer.
    ///
    /// # Safety
    ///
    /// This function must be called only by the inbox's single owner consumer.
    pub(super) unsafe fn take_fifo(&self) -> *mut CoroutineHeader {
        let stack = unsafe {
            // The executor owner is the only consumer. A null result can also
            // mean that the retired head is waiting for an in-flight publisher.
            self.publication.take_graced_stack()
        };
        unsafe {
            // Completed epoch grace transfers every retained pointer provenance
            // with the detached stack to this single consumer.
            reverse(stack, self.kind)
        }
    }

    /// Removes the next link from a detached list node.
    ///
    /// # Safety
    ///
    /// The caller must exclusively own `header` in a detached list of `kind`.
    pub(super) unsafe fn take_next(
        header: *mut CoroutineHeader,
        kind: InboxKind,
    ) -> *mut CoroutineHeader {
        unsafe {
            // Caller owns a detached node from the selected inbox.
            (*header)
                .next(kind)
                .swap(ptr::null_mut(), Ordering::Relaxed)
        }
    }

    #[cfg(test)]
    pub(super) fn arm_test_publisher_pause(&self) {
        self.publication.arm_test_publisher_pause();
    }

    #[cfg(test)]
    pub(super) fn wait_for_test_publisher_pause(&self) {
        self.publication.wait_for_test_publisher_pause();
    }

    #[cfg(test)]
    pub(super) fn resume_test_publisher(&self) {
        self.publication.resume_test_publisher();
    }
}

/// Reverses an exclusively detached intrusive list.
///
/// # Safety
///
/// Every node reachable from `cursor` must be live and exclusively owned by the
/// single consumer through the intrusive link selected by `kind`.
unsafe fn reverse(mut cursor: *mut CoroutineHeader, kind: InboxKind) -> *mut CoroutineHeader {
    let mut reversed = ptr::null_mut();
    while !cursor.is_null() {
        let next = unsafe {
            // The detached list is exclusively owned by the consumer.
            (*cursor).next(kind).load(Ordering::Relaxed)
        };
        unsafe {
            // No producer can access this node until a future publication, which
            // requires it to leave the detached list first.
            (*cursor).next(kind).store(reversed, Ordering::Relaxed);
        }
        reversed = cursor;
        cursor = next;
    }
    reversed
}
