//! Resident classification and historical watermark state.
//!
//! Current RSS is advanced exclusively by the typed resident delta in the
//! address space's mutation receipt. `MappingSlot` remains the installed-page
//! ownership source, while the receipt is the only publication path that may
//! change the current counters. This module owns only the historical high-water
//! mark required by Linux `VmHWM`/`ru_maxrss`.

use core::sync::atomic::{AtomicU64, Ordering};

/// Resident page category matching Linux `MM_*PAGES` buckets.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RssKind {
    Anon = 1,
    File = 2,
    Shmem = 3,
}

impl RssKind {
    pub(crate) const fn from_slot_value(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Anon),
            2 => Some(Self::File),
            3 => Some(Self::Shmem),
            _ => None,
        }
    }

    pub(crate) const fn slot_value(kind: Option<Self>) -> u8 {
        match kind {
            Some(kind) => kind as u8,
            None => 0,
        }
    }
}

/// Historical RSS peak for one address space.
///
/// This is not a current resident counter. Callers first publish a coherent
/// receipt delta, advance the address-space counters, and then observe the new
/// total here.
pub(crate) struct ResidentWatermark {
    hiwater_pages: AtomicU64,
}

impl ResidentWatermark {
    pub(crate) const fn new() -> Self {
        Self {
            hiwater_pages: AtomicU64::new(0),
        }
    }

    pub(crate) fn observe_resident_total(&self, total: u64) -> u64 {
        let mut hiwater = self.hiwater_pages.load(Ordering::Acquire);
        while total > hiwater {
            match self.hiwater_pages.compare_exchange_weak(
                hiwater,
                total,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return total,
                Err(observed) => hiwater = observed,
            }
        }
        hiwater
    }

    pub(crate) fn hiwater_pages(&self) -> u64 {
        self.hiwater_pages.load(Ordering::Acquire)
    }

    /// Resets image-local history when an unpublished address space is reused
    /// by the loader. Current RSS is already empty because every slot was
    /// detached before this call.
    pub(crate) fn reset(&self) {
        self.hiwater_pages.store(0, Ordering::Release);
    }
}

impl Default for ResidentWatermark {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn rss_kind_slot_encoding_is_total_and_typed() {
        for kind in [RssKind::Anon, RssKind::File, RssKind::Shmem] {
            assert_eq!(
                RssKind::from_slot_value(RssKind::slot_value(Some(kind))),
                Some(kind)
            );
        }
        assert_eq!(RssKind::from_slot_value(0), None);
        assert_eq!(RssKind::from_slot_value(u8::MAX), None);
    }

    #[test]
    fn resident_watermark_is_monotonic_until_image_reset() {
        let watermark = ResidentWatermark::new();
        assert_eq!(watermark.observe_resident_total(2), 2);
        assert_eq!(watermark.observe_resident_total(1), 2);
        assert_eq!(watermark.observe_resident_total(7), 7);
        watermark.reset();
        assert_eq!(watermark.observe_resident_total(0), 0);
    }
}
