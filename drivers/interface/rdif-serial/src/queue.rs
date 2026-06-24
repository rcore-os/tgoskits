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

/// Single-producer single-consumer ring.
///
/// One slot is reserved to distinguish full from empty, so the effective
/// capacity is `N - 1`.
pub struct SpscRing<T, const N: usize> {
    slots: [Slot<T>; N],
    head: AtomicUsize,
    tail: AtomicUsize,
}

unsafe impl<T: Send, const N: usize> Send for SpscRing<T, N> {}
unsafe impl<T: Send, const N: usize> Sync for SpscRing<T, N> {}

impl<T, const N: usize> SpscRing<T, N> {
    pub fn new() -> Self {
        assert!(N >= 2);
        Self {
            slots: [const { Slot::uninit() }; N],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        N - 1
    }

    pub fn push(&self, value: T) -> Result<(), T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let next_tail = Self::advance(tail);
        if next_tail == self.head.load(Ordering::Acquire) {
            return Err(value);
        }

        unsafe { (*self.slots[tail].0.get()).write(value) };
        self.tail.store(next_tail, Ordering::Release);
        Ok(())
    }

    pub fn pop(&self) -> Option<T> {
        let head = self.head.load(Ordering::Relaxed);
        if head == self.tail.load(Ordering::Acquire) {
            return None;
        }

        let value = unsafe { (*self.slots[head].0.get()).assume_init_read() };
        self.head.store(Self::advance(head), Ordering::Release);
        Some(value)
    }

    pub fn peek_copy(&self) -> Option<T>
    where
        T: Copy,
    {
        let head = self.head.load(Ordering::Relaxed);
        if head == self.tail.load(Ordering::Acquire) {
            return None;
        }

        Some(unsafe { *(*self.slots[head].0.get()).assume_init_ref() })
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) == self.tail.load(Ordering::Acquire)
    }

    pub fn clear_consumer(&self) {
        while self.pop().is_some() {}
    }

    pub fn len_snapshot(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        if tail >= head {
            tail - head
        } else {
            N - head + tail
        }
    }

    pub fn remaining_snapshot(&self) -> usize {
        self.capacity().saturating_sub(self.len_snapshot())
    }

    const fn advance(index: usize) -> usize {
        let next = index + 1;
        if next == N { 0 } else { next }
    }
}

impl<T, const N: usize> Default for SpscRing<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Drop for SpscRing<T, N> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::SpscRing;

    #[test]
    fn keeps_one_slot_empty() {
        let ring = SpscRing::<u8, 4>::new();
        assert_eq!(ring.capacity(), 3);
        assert_eq!(ring.push(1), Ok(()));
        assert_eq!(ring.push(2), Ok(()));
        assert_eq!(ring.push(3), Ok(()));
        assert_eq!(ring.push(4), Err(4));
        assert_eq!(ring.pop(), Some(1));
        assert_eq!(ring.pop(), Some(2));
        assert_eq!(ring.pop(), Some(3));
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn peek_does_not_consume() {
        let ring = SpscRing::<u8, 3>::new();
        ring.push(7).unwrap();
        assert_eq!(ring.peek_copy(), Some(7));
        assert_eq!(ring.peek_copy(), Some(7));
        assert_eq!(ring.pop(), Some(7));
    }
}
