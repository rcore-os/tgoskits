use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Slot<T>(UnsafeCell<MaybeUninit<T>>);

unsafe impl<T: Send> Send for Slot<T> {}
unsafe impl<T: Send> Sync for Slot<T> {}

impl<T> Slot<T> {
    const fn uninit() -> Self {
        Self(UnsafeCell::new(MaybeUninit::uninit()))
    }
}

struct Ring<T> {
    slots: Box<[Slot<T>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: a ring is created with exactly one producer and one consumer. The
// producer exclusively writes the tail slot, the consumer exclusively reads
// the head slot, and Acquire/Release publication protects initialized values.
unsafe impl<T: Send> Send for Ring<T> {}
// SAFETY: the SPSC endpoint ownership and atomic indices serialize all access
// to the interior-mutable slots as described above.
unsafe impl<T: Send> Sync for Ring<T> {}

impl<T> Ring<T> {
    fn advance(&self, index: usize) -> usize {
        let next = index + 1;
        if next == self.slots.len() { 0 } else { next }
    }
}

impl<T> Drop for Ring<T> {
    fn drop(&mut self) {
        let mut head = *self.head.get_mut();
        let tail = *self.tail.get_mut();
        while head != tail {
            // SAFETY: exclusive `Ring` ownership proves no endpoint remains,
            // and every slot in [head, tail) was initialized by the producer.
            unsafe { (*self.slots[head].0.get()).assume_init_drop() };
            head = self.advance(head);
        }
    }
}

/// The unique producer endpoint of a runtime-private SPSC ring.
pub(super) struct Producer<T> {
    ring: Arc<Ring<T>>,
}

impl<T> Producer<T> {
    pub(super) fn push(&mut self, item: T) -> Result<(), T> {
        let tail = self.ring.tail.load(Ordering::Relaxed);
        let next = self.ring.advance(tail);
        if next == self.ring.head.load(Ordering::Acquire) {
            return Err(item);
        }
        // SAFETY: this endpoint is the only producer and therefore owns the
        // current tail slot until the Release publication below.
        unsafe { (*self.ring.slots[tail].0.get()).write(item) };
        self.ring.tail.store(next, Ordering::Release);
        Ok(())
    }

    pub(super) fn write_room(&self) -> usize {
        let head = self.ring.head.load(Ordering::Acquire);
        let tail = self.ring.tail.load(Ordering::Relaxed);
        if tail >= head {
            self.ring.slots.len() - (tail - head) - 1
        } else {
            head - tail - 1
        }
    }
}

/// The unique consumer endpoint of a runtime-private SPSC ring.
pub(super) struct Consumer<T> {
    ring: Arc<Ring<T>>,
}

impl<T> Consumer<T> {
    pub(super) fn pop(&mut self) -> Option<T> {
        let head = self.ring.head.load(Ordering::Relaxed);
        if head == self.ring.tail.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: this endpoint is the only consumer and owns the current head
        // slot after observing the producer's Release publication.
        let item = unsafe { (*self.ring.slots[head].0.get()).assume_init_read() };
        self.ring
            .head
            .store(self.ring.advance(head), Ordering::Release);
        Some(item)
    }

    pub(super) fn drain(&mut self, out: &mut [T]) -> usize {
        let mut count = 0;
        for slot in out {
            let Some(item) = self.pop() else {
                break;
            };
            *slot = item;
            count += 1;
        }
        count
    }

    pub(super) fn is_empty(&self) -> bool {
        self.ring.head.load(Ordering::Relaxed) == self.ring.tail.load(Ordering::Acquire)
    }

    pub(super) fn clear(&mut self) {
        while self.pop().is_some() {}
    }
}

pub(super) fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    assert!(capacity > 0, "SPSC capacity must be non-zero");
    let mut slots = Vec::with_capacity(capacity + 1);
    slots.resize_with(capacity + 1, Slot::uninit);
    let ring = Arc::new(Ring {
        slots: slots.into_boxed_slice(),
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
    (Producer { ring: ring.clone() }, Consumer { ring })
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::thread;

    use super::*;

    #[test]
    fn preserves_order_across_wraparound() {
        let (mut producer, mut consumer) = channel(4);
        for value in 0..4 {
            producer.push(value).unwrap();
        }
        assert_eq!(producer.push(4), Err(4));
        assert_eq!(consumer.pop(), Some(0));
        assert_eq!(consumer.pop(), Some(1));
        producer.push(4).unwrap();
        producer.push(5).unwrap();

        let mut out = [0; 4];
        assert_eq!(consumer.drain(&mut out), 4);
        assert_eq!(out, [2, 3, 4, 5]);
        assert!(consumer.is_empty());
    }

    #[test]
    fn producer_and_consumer_publish_concurrently() {
        const COUNT: usize = 16_384;
        let (mut producer, mut consumer) = channel(257);
        let producer = thread::spawn(move || {
            for value in 0..COUNT {
                let mut pending = value;
                loop {
                    match producer.push(pending) {
                        Ok(()) => break,
                        Err(value) => {
                            pending = value;
                            thread::yield_now();
                        }
                    }
                }
            }
        });

        for expected in 0..COUNT {
            loop {
                if let Some(value) = consumer.pop() {
                    assert_eq!(value, expected);
                    break;
                }
                thread::yield_now();
            }
        }
        producer.join().unwrap();
        assert!(consumer.is_empty());
    }

    #[test]
    fn exposes_exact_usable_capacity() {
        let (mut producer, _consumer) = channel(16_384);
        for value in 0..16_384 {
            producer.push(value).unwrap();
        }
        assert_eq!(producer.write_room(), 0);
        assert_eq!(producer.push(16_384), Err(16_384));
    }
}
