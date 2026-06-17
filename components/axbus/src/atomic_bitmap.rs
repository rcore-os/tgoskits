use core::sync::atomic::{AtomicU64, Ordering};

/// Lock-free bitmap backed by `AtomicU64` words.
///
/// Designed for interrupt pending/active state that must be updated from
/// any context (including interrupt handlers) without holding locks.
///
/// - `set` / `clear`: single atomic instruction, safe to call concurrently
/// - `test_and_clear`: CAS-free (uses `fetch_and`), returns previous value
/// - `scan_set_bits`: iterates set bits in a snapshot (non-atomic across words)
///
/// Typical usage: `AtomicBitmap<16>` = 1024 bits (RISC-V PLIC source count).
pub struct AtomicBitmap<const WORDS: usize> {
    words: [AtomicU64; WORDS],
}

impl<const W: usize> AtomicBitmap<W> {
    pub const BITS: usize = W * 64;

    pub const fn new() -> Self {
        Self {
            words: [const { AtomicU64::new(0) }; W],
        }
    }

    pub fn set(&self, bit: usize) {
        debug_assert!(bit < Self::BITS);
        let (word, mask) = Self::pos(bit);
        self.words[word].fetch_or(mask, Ordering::Release);
    }

    pub fn clear(&self, bit: usize) {
        debug_assert!(bit < Self::BITS);
        let (word, mask) = Self::pos(bit);
        self.words[word].fetch_and(!mask, Ordering::Release);
    }

    pub fn test(&self, bit: usize) -> bool {
        debug_assert!(bit < Self::BITS);
        let (word, mask) = Self::pos(bit);
        self.words[word].load(Ordering::Acquire) & mask != 0
    }

    /// Atomically test and clear a bit. Returns `true` if the bit was
    /// previously set (i.e., this caller "won" the race).
    pub fn test_and_clear(&self, bit: usize) -> bool {
        debug_assert!(bit < Self::BITS);
        let (word, mask) = Self::pos(bit);
        self.words[word].fetch_and(!mask, Ordering::AcqRel) & mask != 0
    }

    /// Atomically test and set a bit. Returns `true` if the bit was
    /// already set (i.e., duplicate set).
    pub fn test_and_set(&self, bit: usize) -> bool {
        debug_assert!(bit < Self::BITS);
        let (word, mask) = Self::pos(bit);
        self.words[word].fetch_or(mask, Ordering::AcqRel) & mask != 0
    }

    /// Load a single word snapshot.
    pub fn load_word(&self, word_idx: usize) -> u64 {
        self.words[word_idx].load(Ordering::Acquire)
    }

    /// Atomically OR a mask into a single word. Used for bulk bit-set
    /// operations (e.g., MMIO register write of 32 pending bits).
    pub fn or_word(&self, word_idx: usize, mask: u64) {
        self.words[word_idx].fetch_or(mask, Ordering::Release);
    }

    /// Snapshot of `pending & !active` across all words.
    /// Not atomic across words, but each word read is atomic.
    pub fn and_not(&self, other: &Self) -> [u64; W] {
        let mut result = [0u64; W];
        for i in 0..W {
            let a = self.words[i].load(Ordering::Acquire);
            let b = other.words[i].load(Ordering::Acquire);
            result[i] = a & !b;
        }
        result
    }

    /// Returns `true` if any bit is set.
    pub fn any(&self) -> bool {
        for i in 0..W {
            if self.words[i].load(Ordering::Acquire) != 0 {
                return true;
            }
        }
        false
    }

    /// Iterate over all set bit indices in a snapshot.
    pub fn iter_set_bits(&self) -> SetBitIter<W> {
        let mut snapshot = [0u64; W];
        for i in 0..W {
            snapshot[i] = self.words[i].load(Ordering::Acquire);
        }
        SetBitIter {
            snapshot,
            word_idx: 0,
        }
    }

    const fn pos(bit: usize) -> (usize, u64) {
        (bit / 64, 1u64 << (bit % 64))
    }
}

impl<const W: usize> Default for AtomicBitmap<W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const W: usize> core::fmt::Debug for AtomicBitmap<W> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let count: u32 = (0..W)
            .map(|i| self.words[i].load(Ordering::Relaxed).count_ones())
            .sum();
        write!(f, "AtomicBitmap<{}>({}bits set)", Self::BITS, count)
    }
}

// AtomicU64 is Send+Sync, so AtomicBitmap is too.
// (Compiler derives this automatically, but stating it explicitly for clarity.)

pub struct SetBitIter<const W: usize> {
    snapshot: [u64; W],
    word_idx: usize,
}

impl<const W: usize> Iterator for SetBitIter<W> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        while self.word_idx < W {
            let word = self.snapshot[self.word_idx];
            if word != 0 {
                let bit = word.trailing_zeros() as usize;
                self.snapshot[self.word_idx] &= !(1u64 << bit);
                return Some(self.word_idx * 64 + bit);
            }
            self.word_idx += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_set_clear_test() {
        let bm = AtomicBitmap::<2>::new();
        assert!(!bm.test(0));
        assert!(!bm.test(127));

        bm.set(0);
        bm.set(63);
        bm.set(64);
        bm.set(127);

        assert!(bm.test(0));
        assert!(bm.test(63));
        assert!(bm.test(64));
        assert!(bm.test(127));
        assert!(!bm.test(1));

        bm.clear(63);
        assert!(!bm.test(63));
    }

    #[test]
    fn test_and_clear_returns_previous() {
        let bm = AtomicBitmap::<1>::new();
        bm.set(5);

        assert!(bm.test_and_clear(5));
        assert!(!bm.test(5));
        assert!(!bm.test_and_clear(5));
    }

    #[test]
    fn test_and_set_returns_previous() {
        let bm = AtomicBitmap::<1>::new();

        assert!(!bm.test_and_set(10));
        assert!(bm.test(10));
        assert!(bm.test_and_set(10));
    }

    #[test]
    fn and_not_snapshot() {
        let pending = AtomicBitmap::<2>::new();
        let active = AtomicBitmap::<2>::new();

        pending.set(1);
        pending.set(5);
        pending.set(10);
        active.set(5);

        let result = pending.and_not(&active);
        assert!(result[0] & (1 << 1) != 0);
        assert!(result[0] & (1 << 5) == 0);
        assert!(result[0] & (1 << 10) != 0);
    }

    #[test]
    fn any_empty_and_nonempty() {
        let bm = AtomicBitmap::<2>::new();
        assert!(!bm.any());

        bm.set(100);
        assert!(bm.any());

        bm.clear(100);
        assert!(!bm.any());
    }

    #[test]
    fn iter_set_bits_all_found() {
        let bm = AtomicBitmap::<2>::new();
        bm.set(0);
        bm.set(3);
        bm.set(64);
        bm.set(127);

        let bits: alloc::vec::Vec<usize> = bm.iter_set_bits().collect();
        assert_eq!(bits, alloc::vec![0, 3, 64, 127]);
    }

    #[test]
    fn iter_set_bits_empty() {
        let bm = AtomicBitmap::<4>::new();
        assert_eq!(bm.iter_set_bits().count(), 0);
    }

    #[test]
    fn concurrent_set_no_lost_updates() {
        use alloc::sync::Arc;
        use std::thread;

        let bm = Arc::new(AtomicBitmap::<1>::new());
        let mut handles = alloc::vec::Vec::new();

        for bit in 0..64 {
            let bm = bm.clone();
            handles.push(thread::spawn(move || {
                bm.set(bit);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        for bit in 0..64 {
            assert!(bm.test(bit), "bit {bit} should be set");
        }
    }

    #[test]
    fn concurrent_test_and_clear_exactly_one_wins() {
        use alloc::sync::Arc;
        use core::sync::atomic::AtomicUsize;
        use std::thread;

        let bm = Arc::new(AtomicBitmap::<1>::new());
        bm.set(42);

        let winners = Arc::new(AtomicUsize::new(0));
        let mut handles = alloc::vec::Vec::new();

        for _ in 0..16 {
            let bm = bm.clone();
            let winners = winners.clone();
            handles.push(thread::spawn(move || {
                if bm.test_and_clear(42) {
                    winners.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(winners.load(Ordering::Relaxed), 1);
        assert!(!bm.test(42));
    }

    #[test]
    fn debug_format() {
        let bm = AtomicBitmap::<2>::new();
        bm.set(1);
        bm.set(100);
        let s = alloc::format!("{bm:?}");
        assert!(s.contains("2bits set"));
    }

    #[test]
    fn large_bitmap_1024_bits() {
        let bm = AtomicBitmap::<16>::new();
        assert_eq!(AtomicBitmap::<16>::BITS, 1024);

        bm.set(0);
        bm.set(511);
        bm.set(1023);

        assert!(bm.test(0));
        assert!(bm.test(511));
        assert!(bm.test(1023));
        assert!(!bm.test(512));

        let bits: alloc::vec::Vec<usize> = bm.iter_set_bits().collect();
        assert_eq!(bits, alloc::vec![0, 511, 1023]);
    }
}
