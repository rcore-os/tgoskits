//! Embedded intrusive node with reserved payload storage.

use core::{
    cell::UnsafeCell,
    marker::PhantomPinned,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

use super::{InboxKind, InboxMessage};

/// Node embedded separately for wake, migration, and reclaim publication.
#[derive(Debug)]
pub struct InboxNode {
    kind: InboxKind,
    next: AtomicPtr<Self>,
    queued: AtomicBool,
    message: UnsafeCell<InboxMessage>,
    _pin: PhantomPinned,
}

impl InboxNode {
    /// Creates a detached node dedicated to one inbox class.
    pub const fn new(kind: InboxKind) -> Self {
        Self {
            kind,
            next: AtomicPtr::new(ptr::null_mut()),
            queued: AtomicBool::new(false),
            message: UnsafeCell::new(InboxMessage::EMPTY),
            _pin: PhantomPinned,
        }
    }

    /// Returns the inbox class permanently assigned to this node.
    pub const fn kind(&self) -> InboxKind {
        self.kind
    }

    pub(super) fn reserve(&self, message: InboxMessage) -> bool {
        if self
            .queued
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        unsafe {
            // Successful reservation excludes all producers and consumers until
            // Release publication followed by owner-side `take_message`.
            *self.message.get() = message;
        }
        true
    }

    pub(super) const fn next(&self) -> &AtomicPtr<Self> {
        &self.next
    }

    /// Removes the next link from an exclusively detached list node.
    ///
    /// # Safety
    ///
    /// The caller must exclusively own this node through a detached inbox list.
    pub(super) unsafe fn take_next(&self) -> *mut Self {
        self.next.swap(ptr::null_mut(), Ordering::Relaxed)
    }

    /// Copies the reserved message and releases this node for reuse.
    ///
    /// # Safety
    ///
    /// The caller must exclusively own this node through a detached inbox list
    /// and must have removed its next link before this call.
    pub(super) unsafe fn take_message(&self) -> InboxMessage {
        let message = unsafe {
            // Acquire detachment observes the producer payload and queued excludes
            // mutation until this consumer releases the node below.
            *self.message.get()
        };
        self.queued.store(false, Ordering::Release);
        message
    }
}

// SAFETY: Cross-CPU access is restricted to atomics while queued. UnsafeCell
// payload access is guarded by the reservation/publication ownership protocol.
unsafe impl Send for InboxNode {}
// SAFETY: Producers serialize payload writes through queued CAS and the consumer
// reads only after Acquire detachment, before releasing queued.
unsafe impl Sync for InboxNode {}
