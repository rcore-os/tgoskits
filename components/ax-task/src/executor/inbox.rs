//! Intrusive multi-producer, single-consumer coroutine inboxes.

use core::{
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use super::CoroutineHeader;

#[derive(Clone, Copy)]
pub(super) enum InboxKind {
    Ready,
}

pub(super) struct IntrusiveInbox {
    head: AtomicPtr<CoroutineHeader>,
    kind: InboxKind,
}

impl IntrusiveInbox {
    pub(super) const fn new(kind: InboxKind) -> Self {
        Self {
            head: AtomicPtr::new(ptr::null_mut()),
            kind,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire).is_null()
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
        let mut observed = self.head.load(Ordering::Relaxed);
        loop {
            next.store(observed, Ordering::Relaxed);
            match self.head.compare_exchange_weak(
                observed,
                header,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(updated) => observed = updated,
            }
        }
    }

    /// Detaches and FIFO-orders every node currently visible to the consumer.
    ///
    /// # Safety
    ///
    /// This function must be called only by the inbox's single owner consumer.
    pub(super) unsafe fn take_fifo(&self) -> *mut CoroutineHeader {
        let stack = self.head.swap(ptr::null_mut(), Ordering::Acquire);
        unsafe {
            // The single consumer owns the entire detached stack and may reverse
            // its links to recover FIFO order.
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
