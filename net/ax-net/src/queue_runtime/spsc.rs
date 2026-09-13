use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::MaybeUninit,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Heap-backed SPSC core.  One slot is reserved to distinguish full/empty.
struct SpscCore<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: only the unique producer writes at `tail`, only the unique consumer
// reads/drops at `head`, and Acquire/Release publication prevents aliasing a
// live slot. `T: Send` is the required cross-CPU ownership contract.
unsafe impl<T: Send> Sync for SpscCore<T> {}
unsafe impl<T: Send> Send for SpscCore<T> {}

impl<T> Drop for SpscCore<T> {
    fn drop(&mut self) {
        let mut head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        while head != tail {
            // SAFETY: after both endpoints are dropped no concurrent access
            // remains, and every index in [head, tail) contains one live item.
            unsafe { (*self.slots[head].get()).assume_init_drop() };
            head = (head + 1) % self.slots.len();
        }
    }
}

pub(super) struct SpscProducer<T> {
    core: Arc<SpscCore<T>>,
    _not_sync: PhantomData<Cell<()>>,
}

pub(super) struct SpscConsumer<T> {
    core: Arc<SpscCore<T>>,
    _not_sync: PhantomData<Cell<()>>,
}

pub(super) fn spsc_ring<T: Send>(capacity: usize) -> (SpscProducer<T>, SpscConsumer<T>) {
    let slots = (0..capacity.saturating_add(1).max(2))
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let core = Arc::new(SpscCore {
        slots,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
    (
        SpscProducer {
            core: Arc::clone(&core),
            _not_sync: PhantomData,
        },
        SpscConsumer {
            core,
            _not_sync: PhantomData,
        },
    )
}

impl<T> SpscProducer<T> {
    pub(super) fn push(&mut self, value: T) -> Result<(), T> {
        let tail = self.core.tail.load(Ordering::Relaxed);
        let next = (tail + 1) % self.core.slots.len();
        if next == self.core.head.load(Ordering::Acquire) {
            return Err(value);
        }
        // SAFETY: only this producer can own the unpublished `tail` slot.
        unsafe { (*self.core.slots[tail].get()).write(value) };
        self.core.tail.store(next, Ordering::Release);
        Ok(())
    }
}

impl<T> SpscConsumer<T> {
    pub(super) fn pop(&mut self) -> Option<T> {
        let head = self.core.head.load(Ordering::Relaxed);
        if head == self.core.tail.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: the producer published this slot with Release and cannot
        // reuse it until this consumer advances `head`.
        let value = unsafe { (*self.core.slots[head].get()).assume_init_read() };
        self.core
            .head
            .store((head + 1) % self.core.slots.len(), Ordering::Release);
        Some(value)
    }
}
