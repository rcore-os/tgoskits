use alloc::collections::VecDeque;

pub struct FixedQueue<T, const CAP: usize> {
    queue: VecDeque<T>,
}

impl<T, const CAP: usize> FixedQueue<T, CAP> {
    pub fn new() -> Self {
        assert!(CAP > 0);
        Self {
            queue: VecDeque::with_capacity(CAP),
        }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn remaining(&self) -> usize {
        CAP - self.queue.len()
    }

    pub fn front(&self) -> Option<&T> {
        self.queue.front()
    }

    pub fn pop_front(&mut self) -> Option<T> {
        self.queue.pop_front()
    }

    pub fn push_back(&mut self, value: T) -> Result<(), T> {
        if self.queue.len() == CAP {
            Err(value)
        } else {
            self.queue.push_back(value);
            Ok(())
        }
    }

    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

impl<T, const CAP: usize> Default for FixedQueue<T, CAP> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAP: usize> FixedQueue<u8, CAP> {
    pub fn push_slice(&mut self, bytes: &[u8]) -> usize {
        let count = bytes.len().min(self.remaining());
        self.queue.extend(bytes[..count].iter().copied());
        count
    }
}
