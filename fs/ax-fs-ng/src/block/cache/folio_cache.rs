//! Fallible fixed-capacity index and LRU order for cached folios.

use core::num::NonZeroUsize;

use hashbrown::HashMap;

use crate::{BlockError, BlockResult};

struct CachedFolio<T> {
    folio: T,
    more_recent: Option<u64>,
    less_recent: Option<u64>,
}

/// Fixed-capacity hash index with an allocation-free intrusive LRU order.
///
/// Hash-table growth is reserved fallibly immediately before insertion.
/// The links use frame indices rather than pointers so rehashing cannot
/// invalidate the LRU order.
pub(super) struct FolioCache<T> {
    entries: HashMap<u64, CachedFolio<T>>,
    capacity: NonZeroUsize,
    most_recent: Option<u64>,
    least_recent: Option<u64>,
}

impl<T> FolioCache<T> {
    pub(super) fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            most_recent: None,
            least_recent: None,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(super) fn contains(&self, frame: &u64) -> bool {
        self.entries.contains_key(frame)
    }

    pub(super) fn is_full(&self) -> bool {
        self.entries.len() >= self.capacity.get()
    }

    pub(super) fn get(&self, frame: &u64) -> Option<&T> {
        self.entries.get(frame).map(|entry| &entry.folio)
    }

    pub(super) fn get_mut(&mut self, frame: &u64) -> Option<&mut T> {
        self.touch(*frame);
        self.entries.get_mut(frame).map(|entry| &mut entry.folio)
    }

    pub(super) fn touch(&mut self, frame: u64) {
        if self.most_recent == Some(frame) {
            return;
        }
        let Some(entry) = self.entries.get(&frame) else {
            return;
        };
        let more_recent = entry.more_recent;
        let less_recent = entry.less_recent;

        if let Some(more_recent) = more_recent {
            self.entries
                .get_mut(&more_recent)
                .expect("LRU link must name a cached folio")
                .less_recent = less_recent;
        } else {
            self.most_recent = less_recent;
        }
        if let Some(less_recent) = less_recent {
            self.entries
                .get_mut(&less_recent)
                .expect("LRU link must name a cached folio")
                .more_recent = more_recent;
        } else {
            self.least_recent = more_recent;
        }

        let previous_most_recent = self.most_recent;
        let entry = self
            .entries
            .get_mut(&frame)
            .expect("touched folio must remain cached");
        entry.more_recent = None;
        entry.less_recent = previous_most_recent;
        if let Some(previous_most_recent) = previous_most_recent {
            self.entries
                .get_mut(&previous_most_recent)
                .expect("LRU head must name a cached folio")
                .more_recent = Some(frame);
        } else {
            self.least_recent = Some(frame);
        }
        self.most_recent = Some(frame);
    }

    /// Reserves the hash-table slot needed by a later insertion. Callers do
    /// this before evicting so allocation failure cannot change LRU state.
    pub(super) fn try_reserve_entry(&mut self) -> BlockResult<()> {
        self.entries
            .try_reserve(1)
            .map_err(|_| BlockError::NoMemory)
    }

    #[cfg(test)]
    pub(super) fn try_reserve_for_test(&mut self, additional: usize) -> BlockResult<()> {
        self.entries
            .try_reserve(additional)
            .map_err(|_| BlockError::NoMemory)
    }

    /// Inserts after [`Self::try_reserve_entry`] has succeeded.
    pub(super) fn insert_reserved(&mut self, frame: u64, folio: T) {
        debug_assert!(!self.entries.contains_key(&frame));
        debug_assert!(!self.is_full());
        let previous_most_recent = self.most_recent;
        self.entries.insert(
            frame,
            CachedFolio {
                folio,
                more_recent: None,
                less_recent: previous_most_recent,
            },
        );
        if let Some(previous_most_recent) = previous_most_recent {
            self.entries
                .get_mut(&previous_most_recent)
                .expect("LRU head must name a cached folio")
                .more_recent = Some(frame);
        } else {
            self.least_recent = Some(frame);
        }
        self.most_recent = Some(frame);
    }

    pub(super) fn remove(&mut self, frame: &u64) -> Option<T> {
        let entry = self.entries.remove(frame)?;
        if let Some(more_recent) = entry.more_recent {
            self.entries
                .get_mut(&more_recent)
                .expect("LRU link must name a cached folio")
                .less_recent = entry.less_recent;
        } else {
            self.most_recent = entry.less_recent;
        }
        if let Some(less_recent) = entry.less_recent {
            self.entries
                .get_mut(&less_recent)
                .expect("LRU link must name a cached folio")
                .more_recent = entry.more_recent;
        } else {
            self.least_recent = entry.more_recent;
        }
        Some(entry.folio)
    }

    pub(super) fn least_recent(&self) -> Option<u64> {
        self.least_recent
    }

    pub(super) fn frames(&self) -> impl Iterator<Item = u64> + '_ {
        self.entries.keys().copied()
    }
}
