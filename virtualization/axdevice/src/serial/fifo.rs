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

    const fn available(&self) -> usize {
        N - self.len
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

/// Receive storage with one bounded backend read reserved outside the UART lock.
///
/// Reserving space also serializes backend reads. Register reads may consume
/// bytes while a read is in flight; loopback writes cannot take its capacity.
pub(super) struct RxFifo<const N: usize> {
    fifo: ByteFifo<N>,
    reserved: usize,
}

impl<const N: usize> RxFifo<N> {
    pub(super) const fn new() -> Self {
        Self {
            fifo: ByteFifo::new(),
            reserved: 0,
        }
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.fifo.is_empty()
    }

    pub(super) const fn is_full(&self) -> bool {
        self.fifo.is_full()
    }

    pub(super) fn begin_read(&mut self, limit: usize) -> Option<usize> {
        if self.reserved != 0 {
            return None;
        }
        let count = limit.min(self.fifo.available());
        if count == 0 {
            return None;
        }
        self.reserved = count;
        Some(count)
    }

    pub(super) fn finish_read(&mut self, bytes: &[u8]) {
        assert!(bytes.len() <= self.reserved);
        for &byte in bytes {
            assert!(self.fifo.push(byte), "backend RX space was reserved");
        }
        self.reserved = 0;
    }

    pub(super) fn push(&mut self, byte: u8) -> bool {
        if self.fifo.available() == self.reserved {
            return false;
        }
        self.fifo.push(byte)
    }

    pub(super) fn pop(&mut self) -> Option<u8> {
        self.fifo.pop()
    }

    pub(super) fn clear(&mut self) {
        // FIFO reset clears received bytes, not an external read in flight.
        // That read commits as a later arrival and retains its reserved space.
        self.fifo.clear();
    }
}
