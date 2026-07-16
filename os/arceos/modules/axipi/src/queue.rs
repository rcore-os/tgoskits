use alloc::boxed::Box;
use core::ptr;

use crate::event::Callback;

/// One caller-allocated callback node linked into a destination FIFO.
pub struct IpiEventNode {
    src_cpu_id: usize,
    callback: Option<Callback>,
    next: *mut Self,
}

impl IpiEventNode {
    /// Allocates a detached node before CPU pinning or queue locking begins.
    pub fn prepare(callback: Callback) -> Box<Self> {
        Box::new(Self {
            src_cpu_id: usize::MAX,
            callback: Some(callback),
            next: ptr::null_mut(),
        })
    }

    /// Removes the callback from a detached node.
    pub fn take_callback(&mut self) -> Callback {
        self.callback
            .take()
            .expect("a detached IPI event node owns one callback")
    }

    pub(crate) fn take_parts(&mut self) -> (usize, Callback) {
        let source = self.src_cpu_id;
        let callback = self.take_callback();
        (source, callback)
    }
}

// SAFETY: a detached node is uniquely owned by its Box. Once linked, every
// pointer mutation is serialized by the destination's SpinNoIrq queue lock;
// the callback itself is Send.
unsafe impl Send for IpiEventNode {}

/// Intrusive FIFO of caller-allocated IPI callback nodes.
///
/// `push` only links an existing node and therefore cannot allocate.
pub struct IpiEventQueue {
    head: *mut IpiEventNode,
    tail: *mut IpiEventNode,
}

impl IpiEventQueue {
    /// Creates an empty callback FIFO.
    pub const fn new() -> Self {
        Self {
            head: ptr::null_mut(),
            tail: ptr::null_mut(),
        }
    }

    /// Returns whether the queue contains no callback node.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.head.is_null()
    }

    /// Links one preallocated node at the FIFO tail without allocating.
    pub fn push(&mut self, src_cpu_id: usize, mut node: Box<IpiEventNode>) {
        debug_assert!(node.next.is_null());
        debug_assert!(node.callback.is_some());
        node.src_cpu_id = src_cpu_id;
        let node = Box::into_raw(node);

        if self.tail.is_null() {
            self.head = node;
        } else {
            // SAFETY: a non-null tail is one live node exclusively owned by
            // this queue, and `&mut self` serializes its link update.
            unsafe { (*self.tail).next = node };
        }
        self.tail = node;
    }

    /// Detaches the FIFO head without allocating.
    #[must_use]
    pub fn pop_node(&mut self) -> Option<Box<IpiEventNode>> {
        let node = self.head;
        if node.is_null() {
            return None;
        }

        // SAFETY: head is a live Box allocation owned exclusively by this
        // queue. Detaching it transfers that unique ownership back to Box.
        let next = unsafe { (*node).next };
        self.head = next;
        if next.is_null() {
            self.tail = ptr::null_mut();
        }
        // Clear the intrusive link before returning the detached node.
        unsafe { (*node).next = ptr::null_mut() };
        Some(unsafe { Box::from_raw(node) })
    }
}

// SAFETY: all pointees are Send and queue mutation requires `&mut self`; the
// owning SpinNoIrq supplies cross-CPU exclusion.
unsafe impl Send for IpiEventQueue {}

impl Drop for IpiEventQueue {
    fn drop(&mut self) {
        while let Some(node) = self.pop_node() {
            drop(node);
        }
    }
}

impl Default for IpiEventQueue {
    fn default() -> Self {
        Self::new()
    }
}
