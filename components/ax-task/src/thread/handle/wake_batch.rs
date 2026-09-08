//! Allocation-free task-context wake batching.

use alloc::{rc::Rc, sync::Arc};
use core::{marker::PhantomData, mem::ManuallyDrop, ptr, sync::atomic::Ordering};

use super::{ThreadCore, ThreadWakeHandle};

/// A task-context wake list backed by nodes embedded in each thread core.
///
/// Queue owners add handles while holding their own metadata lock, release
/// that lock, and then call [`Self::wake_all`]. This is the same ownership
/// split as Linux `wake_q`: unlink and select atomically under the domain lock,
/// perform scheduler wakes afterwards, and never allocate in between.
///
/// A thread can occur at most once in all live batches. Duplicate insertion is
/// coalesced and returns `false`. The batch is deliberately neither `Send` nor
/// `Sync`; the task context that selected the waiters must drain it.
#[must_use = "selected threads must be woken after releasing the domain lock"]
pub struct ThreadWakeBatch {
    head: *const ThreadCore,
    tail: *const ThreadCore,
    len: usize,
    _task_context: PhantomData<Rc<()>>,
}

impl ThreadWakeBatch {
    /// Creates an empty batch without allocating.
    pub const fn new() -> Self {
        Self {
            head: ptr::null(),
            tail: ptr::null(),
            len: 0,
            _task_context: PhantomData,
        }
    }

    /// Adds a wake handle, returning `false` when this thread is already in a
    /// live batch.
    pub fn push(&mut self, wake: ThreadWakeHandle) -> bool {
        let core = &wake.core;
        if core
            .wake_batch_linked
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }

        let raw = Self::into_raw(wake);
        unsafe {
            // SAFETY: `raw` owns one strong reference transferred from `wake`.
            // The successful linked transition gives this batch exclusive
            // access to the embedded link until `pop` clears it.
            (*raw)
                .wake_batch_next
                .store(ptr::null_mut(), Ordering::Relaxed);
            if self.tail.is_null() {
                self.head = raw;
            } else {
                (*self.tail)
                    .wake_batch_next
                    .store(raw.cast_mut(), Ordering::Release);
            }
        }
        self.tail = raw;
        self.len += 1;
        true
    }

    /// Returns the number of unique threads selected by this batch.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether no thread has been selected.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Wakes all selected threads in FIFO order.
    pub fn wake_all(mut self) -> usize {
        let count = self.len;
        while let Some(wake) = self.pop() {
            let _result = wake.wake_from_task();
        }
        count
    }

    fn into_raw(wake: ThreadWakeHandle) -> *const ThreadCore {
        let mut wake = ManuallyDrop::new(wake);
        let core = unsafe {
            // SAFETY: `wake` will not run Drop. Its core ownership is moved to
            // the returned raw Arc and reconstructed exactly once by `pop`.
            ManuallyDrop::take(&mut wake.core)
        };
        let reap_signal = unsafe {
            // SAFETY: identical ownership transfer for the auxiliary Arc. The
            // external lease remains owned by the batch and is released when
            // the reconstructed handle is dropped.
            ptr::read(&wake.reap_signal)
        };
        drop(reap_signal);
        Arc::into_raw(core)
    }

    unsafe fn from_raw(raw: *const ThreadCore) -> ThreadWakeHandle {
        let core = unsafe {
            // SAFETY: every pointer placed in the batch came from one
            // `Arc::into_raw`, and `pop` removes it exactly once.
            Arc::from_raw(raw)
        };
        let reap_signal = Arc::clone(&core.reap_signal);
        ThreadWakeHandle {
            core: ManuallyDrop::new(core),
            reap_signal,
        }
    }

    fn pop(&mut self) -> Option<ThreadWakeHandle> {
        let raw = self.head;
        if raw.is_null() {
            return None;
        }

        let next = unsafe {
            // SAFETY: the raw Arc keeps the core alive and this batch has
            // exclusive ownership of its embedded link.
            (*raw).wake_batch_next.load(Ordering::Acquire).cast_const()
        };
        self.head = next;
        if next.is_null() {
            self.tail = ptr::null();
        }
        self.len -= 1;
        unsafe {
            // SAFETY: clearing the link completes this batch's exclusive node
            // ownership before reconstructing the owning wake handle.
            (*raw)
                .wake_batch_next
                .store(ptr::null_mut(), Ordering::Relaxed);
            (*raw).wake_batch_linked.store(false, Ordering::Release);
            Some(Self::from_raw(raw))
        }
    }
}

impl Default for ThreadWakeBatch {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ThreadWakeBatch {
    fn drop(&mut self) {
        // Dropping an undrained batch is a caller bug, but ownership must still
        // be released without invoking scheduler callbacks from an unknown
        // lock context.
        let was_empty = self.is_empty();
        while let Some(wake) = self.pop() {
            drop(wake);
        }
        debug_assert!(was_empty, "thread wake batch was not drained");
    }
}
