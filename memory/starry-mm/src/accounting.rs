//! Per-address-space resident page counters (Linux `mm_struct.rss_stat` analogue).
//!
//! The mapping backend or resident page owner is authoritative for each page's
//! category. This type stores only aggregate counters and never reconstructs or
//! repairs page classification from a second address-indexed data structure.

use core::sync::atomic::{AtomicU64, Ordering};

use ax_errno::{AxError, AxResult};

/// Resident page category matching Linux `MM_*PAGES` buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RssKind {
    Anon,
    File,
    Shmem,
}

/// Incremental resident-set counters for one address space.
///
/// Mutations are serialized by the owning address-space lock. Atomics allow
/// lock-free statistics snapshots without making them the synchronization
/// mechanism for mapping transactions.
pub struct MemoryAccounting {
    rss_anon: AtomicU64,
    rss_file: AtomicU64,
    rss_shmem: AtomicU64,
    hiwater_rss: AtomicU64,
}

impl Default for MemoryAccounting {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryAccounting {
    pub const fn new() -> Self {
        Self {
            rss_anon: AtomicU64::new(0),
            rss_file: AtomicU64::new(0),
            rss_shmem: AtomicU64::new(0),
            hiwater_rss: AtomicU64::new(0),
        }
    }

    pub fn rss_anon_pages(&self) -> u64 {
        self.rss_anon.load(Ordering::Relaxed)
    }

    pub fn rss_file_pages(&self) -> u64 {
        self.rss_file.load(Ordering::Relaxed)
    }

    pub fn rss_shmem_pages(&self) -> u64 {
        self.rss_shmem.load(Ordering::Relaxed)
    }

    pub fn rss_total_pages(&self) -> u64 {
        self.rss_anon_pages()
            .saturating_add(self.rss_file_pages())
            .saturating_add(self.rss_shmem_pages())
    }

    /// Linux `get_mm_hiwater_rss`: max(stored peak, current total).
    pub fn hiwater_rss_pages(&self) -> u64 {
        self.hiwater_rss
            .load(Ordering::Relaxed)
            .max(self.rss_total_pages())
    }

    /// Adds resident pages after their mappings and ownership records commit.
    pub fn inc(&self, kind: RssKind, pages: u64) -> AxResult {
        if pages == 0 {
            return Ok(());
        }
        self.counter(kind)
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(pages)
            })
            .map_err(|_| AxError::BadState)?;
        self.update_hiwater(self.rss_total_pages());
        Ok(())
    }

    /// Removes resident pages without allowing a release-build underflow.
    pub fn dec(&self, kind: RssKind, pages: u64) -> AxResult {
        if pages == 0 {
            return Ok(());
        }
        self.counter(kind)
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_sub(pages)
            })
            .map(|_| ())
            .map_err(|_| AxError::BadState)
    }

    /// Moves resident pages between categories as one checked state transition.
    pub fn reclassify(&self, from: RssKind, to: RssKind, pages: u64) -> AxResult {
        if from == to || pages == 0 {
            return Ok(());
        }
        self.dec(from, pages)?;
        if let Err(error) = self.inc(to, pages) {
            self.inc(from, pages).map_err(|_| AxError::BadState)?;
            return Err(error);
        }
        Ok(())
    }

    /// Returns a coherent-enough statistics snapshot for `/proc` reporting.
    pub fn snapshot_resident_pages(&self) -> (u64, u64, u64) {
        (
            self.rss_anon_pages(),
            self.rss_file_pages(),
            self.rss_shmem_pages(),
        )
    }

    /// Sets one counter to construct deterministic accounting failures.
    #[doc(hidden)]
    #[cfg(feature = "axtest")]
    pub fn set_resident_pages_for_test(&self, kind: RssKind, pages: u64) {
        self.counter(kind).store(pages, Ordering::Relaxed);
    }

    fn counter(&self, kind: RssKind) -> &AtomicU64 {
        match kind {
            RssKind::Anon => &self.rss_anon,
            RssKind::File => &self.rss_file,
            RssKind::Shmem => &self.rss_shmem,
        }
    }

    fn update_hiwater(&self, total: u64) {
        let mut hiwater = self.hiwater_rss.load(Ordering::Relaxed);
        while total > hiwater {
            match self.hiwater_rss.compare_exchange_weak(
                hiwater,
                total,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => hiwater = observed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inc_dec_and_hiwater() {
        let accounting = MemoryAccounting::new();
        accounting.inc(RssKind::Anon, 4).unwrap();
        assert_eq!(accounting.rss_total_pages(), 4);
        assert_eq!(accounting.hiwater_rss_pages(), 4);
        accounting.inc(RssKind::File, 2).unwrap();
        assert_eq!(accounting.rss_total_pages(), 6);
        accounting.dec(RssKind::Anon, 1).unwrap();
        assert_eq!(accounting.rss_anon_pages(), 3);
        assert_eq!(accounting.hiwater_rss_pages(), 6);
    }

    #[test]
    fn underflow_is_rejected_without_mutating_the_counter() {
        let accounting = MemoryAccounting::new();
        accounting.inc(RssKind::File, 1).unwrap();

        assert_eq!(accounting.dec(RssKind::File, 2), Err(AxError::BadState));
        assert_eq!(accounting.rss_file_pages(), 1);
    }

    #[test]
    fn reclassify_changes_only_the_selected_categories() {
        let accounting = MemoryAccounting::new();
        accounting.inc(RssKind::File, 2).unwrap();
        accounting.inc(RssKind::Shmem, 1).unwrap();

        accounting
            .reclassify(RssKind::File, RssKind::Anon, 1)
            .unwrap();

        assert_eq!(accounting.snapshot_resident_pages(), (1, 1, 1));
        assert_eq!(accounting.hiwater_rss_pages(), 3);
    }
}
