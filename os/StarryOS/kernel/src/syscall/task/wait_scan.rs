//! Refreshable candidate source for Linux-style process waits.

extern crate alloc;

use alloc::vec::Vec;

/// Re-runs one authoritative candidate query for every wait-loop scan.
pub(crate) struct WaitCandidateScan<S> {
    source: S,
}

impl<S> WaitCandidateScan<S> {
    pub(crate) const fn new(source: S) -> Self {
        Self { source }
    }
}

impl<S, C> WaitCandidateScan<S>
where
    S: Fn() -> Vec<C>,
{
    pub(crate) fn collect(&self) -> Vec<C> {
        (self.source)()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::cell::Cell;

    use super::WaitCandidateScan;

    #[test]
    fn refreshes_candidates_after_each_wait_wake() {
        let published = Cell::new(1usize);
        let scan = WaitCandidateScan::new(|| (0..published.get()).collect::<Vec<_>>());

        assert_eq!(scan.collect(), [0]);
        published.set(2);
        assert_eq!(scan.collect(), [0, 1]);
    }
}
