pub(super) struct ByteRing<const N: usize> {
    buf: [u8; N],
    head: usize,
    len: usize,
}

impl<const N: usize> ByteRing<N> {
    pub(super) const fn new() -> Self {
        Self {
            buf: [0; N],
            head: 0,
            len: 0,
        }
    }

    pub(super) fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn push_back(&mut self, byte: u8) -> bool {
        if self.len == N {
            return false;
        }
        let tail = (self.head + self.len) % N;
        self.buf[tail] = byte;
        self.len += 1;
        true
    }

    pub(super) fn pop_front(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.buf[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(byte)
    }

    pub(super) fn drain_into(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        for slot in out.iter_mut().take(n) {
            *slot = self
                .pop_front()
                .expect("ring length was precomputed before draining");
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::ByteRing;

    #[test]
    fn fixed_ring_preserves_fifo_and_drops_when_full() {
        let mut ring = ByteRing::<3>::new();
        assert!(ring.push_back(1));
        assert!(ring.push_back(2));
        assert!(ring.push_back(3));
        assert!(!ring.push_back(4));
        assert_eq!(ring.len(), 3);

        let mut out = [0; 4];
        assert_eq!(ring.drain_into(&mut out), 3);
        assert_eq!(&out[..3], &[1, 2, 3]);
        assert!(ring.is_empty());
    }
}
