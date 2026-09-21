//! Fixed-capacity byte FIFO shared by virtual UART models.

pub(super) struct ByteFifo<const N: usize> {
    bytes: [u8; N],
    head: usize,
    len: usize,
}

impl<const N: usize> ByteFifo<N> {
    pub(super) const fn new() -> Self {
        Self {
            bytes: [0; N],
            head: 0,
            len: 0,
        }
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(super) const fn is_full(&self) -> bool {
        self.len == N
    }

    pub(super) fn copy_to(&self, output: &mut [u8]) -> usize {
        let count = output.len().min(self.len);
        for (index, byte) in output[..count].iter_mut().enumerate() {
            *byte = self.bytes[(self.head + index) % N];
        }
        count
    }

    pub(super) fn discard(&mut self, count: usize) {
        let count = count.min(self.len);
        self.head = (self.head + count) % N;
        self.len -= count;
    }

    pub(super) fn push(&mut self, byte: u8) -> bool {
        if self.is_full() {
            return false;
        }
        let tail = (self.head + self.len) % N;
        self.bytes[tail] = byte;
        self.len += 1;
        true
    }

    pub(super) fn pop(&mut self) -> Option<u8> {
        if self.is_empty() {
            return None;
        }
        let byte = self.bytes[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(byte)
    }

    pub(super) fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}
