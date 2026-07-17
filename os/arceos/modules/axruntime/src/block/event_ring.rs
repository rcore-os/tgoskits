use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RingFull;

struct EventSlot<T: Copy> {
    sequence: AtomicUsize,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T: Copy> EventSlot<T> {
    fn new(sequence: usize) -> Self {
        Self {
            sequence: AtomicUsize::new(sequence),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

// SAFETY: a producer writes a slot only after owning its sequence, and the
// single consumer reads it only after the producer's Release publication.
unsafe impl<T: Copy + Send> Sync for EventSlot<T> {}

/// Fixed-capacity MPSC ring used to carry acknowledged hard-IRQ snapshots.
pub(crate) struct EventRing<T: Copy, const N: usize> {
    enqueue: AtomicUsize,
    dequeue: AtomicUsize,
    slots: [EventSlot<T>; N],
}

impl<T: Copy + Send, const N: usize> EventRing<T, N> {
    pub(crate) fn new() -> Self {
        assert!(N > 0, "IRQ event ring capacity must be non-zero");
        Self {
            enqueue: AtomicUsize::new(0),
            dequeue: AtomicUsize::new(0),
            slots: core::array::from_fn(EventSlot::new),
        }
    }

    /// Publishes one Copy snapshot without allocation, locks, or callbacks.
    pub(crate) fn try_push(&self, event: T) -> Result<(), RingFull> {
        let mut position = self.enqueue.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[position % N];
            let sequence = slot.sequence.load(Ordering::Acquire);
            let difference = sequence.wrapping_sub(position) as isize;
            if difference == 0 {
                match self.enqueue.compare_exchange_weak(
                    position,
                    position.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        unsafe {
                            // SAFETY: this producer exclusively reserved this
                            // sequence and T is Copy, so no drop state exists.
                            (*slot.value.get()).write(event);
                        }
                        slot.sequence
                            .store(position.wrapping_add(1), Ordering::Release);
                        return Ok(());
                    }
                    Err(observed) => position = observed,
                }
            } else if difference < 0 {
                return Err(RingFull);
            } else {
                position = self.enqueue.load(Ordering::Relaxed);
            }
        }
    }

    /// Removes one snapshot from the sole task-context consumer.
    pub(crate) fn pop(&self) -> Option<T> {
        let mut position = self.dequeue.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[position % N];
            let sequence = slot.sequence.load(Ordering::Acquire);
            let expected = position.wrapping_add(1);
            let difference = sequence.wrapping_sub(expected) as isize;
            if difference == 0 {
                match self.dequeue.compare_exchange_weak(
                    position,
                    expected,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        let event = unsafe {
                            // SAFETY: sequence publication proves this slot is
                            // initialized and the sole consumer owns removal.
                            (*slot.value.get()).assume_init_read()
                        };
                        slot.sequence
                            .store(position.wrapping_add(N), Ordering::Release);
                        return Some(event);
                    }
                    Err(observed) => position = observed,
                }
            } else if difference < 0 {
                return None;
            } else {
                position = self.dequeue.load(Ordering::Relaxed);
            }
        }
    }

    /// Returns whether no producer reservation remains ahead of the consumer.
    ///
    /// A producer that reserved a slot but has not published its sequence makes
    /// this conservatively return `false`, which is required by IRQ teardown.
    pub(crate) fn is_empty(&self) -> bool {
        self.enqueue.load(Ordering::Acquire) == self.dequeue.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};

    use super::*;

    #[test]
    fn full_ring_rejects_without_overwriting_unconsumed_events() {
        let ring = EventRing::<u32, 2>::new();
        ring.try_push(1).unwrap();
        ring.try_push(2).unwrap();
        assert_eq!(ring.try_push(3), Err(RingFull));
        assert_eq!(ring.pop(), Some(1));
        assert_eq!(ring.pop(), Some(2));
        assert_eq!(ring.pop(), None);
        assert!(ring.is_empty());
    }

    #[test]
    fn concurrent_irq_producers_publish_each_snapshot_once() {
        const PRODUCERS: usize = 4;
        const PER_PRODUCER: usize = 128;
        let ring = Arc::new(EventRing::<usize, 64>::new());
        let mut producers = Vec::new();
        for producer in 0..PRODUCERS {
            let ring = Arc::clone(&ring);
            producers.push(std::thread::spawn(move || {
                for item in 0..PER_PRODUCER {
                    let value = producer * PER_PRODUCER + item;
                    while ring.try_push(value).is_err() {
                        std::thread::yield_now();
                    }
                }
            }));
        }

        let mut observed = Vec::new();
        while observed.len() != PRODUCERS * PER_PRODUCER {
            if let Some(value) = ring.pop() {
                observed.push(value);
            } else {
                std::thread::yield_now();
            }
        }
        for producer in producers {
            producer.join().unwrap();
        }
        observed.sort_unstable();
        assert_eq!(observed, (0..PRODUCERS * PER_PRODUCER).collect::<Vec<_>>());
    }
}
