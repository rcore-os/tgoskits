//! Per-address-space virtual memory accounting (VmX).
//!
//! [`ProcessVmStat`] owns only virtual-address-space counters. Resident state
//! is derived from published `MappingSlot`s and its historical watermark is
//! owned by `ResidentWatermark`; keeping RSS here as well would create a
//! second fact that can diverge from the installed mapping graph.
//!
//! # Counter categories
//!
//! | Category | Fields | Update rule |
//! |---|---|---|
//! | Current (O(1) atomic) | `vss_pages` | +size on map, -size on unmap/clear |
//! | High-water mark | `peak_vss_pages` | `fetch_max` on map |
//!
//! Current VSS is maintained as an `AtomicI64` (signed) so that a
//! double-unmap or a race never wraps to u64::MAX; it is always read as
//! `max(0, value)`.

use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// All VmX accounting for one address space.
///
/// Fields are intentionally private; use the provided methods to read or
/// update them.  This ensures the monotonicity invariant on the high-water
/// marks is maintained by construction.
pub struct ProcessVmStat {
    // ── Current counters (O(1), updated on every map/unmap) ──────────────
    /// Current virtual size in pages (VmSize).  Signed to catch underflow bugs.
    vss_pages: AtomicI64,

    // ── High-water marks (monotonically non-decreasing) ──────────────────
    /// Peak virtual size in pages (VmPeak).
    peak_vss_pages: AtomicU64,
}

#[derive(Clone, Copy)]
pub(super) struct ProcessVmStatSnapshot {
    vss_pages: i64,
    peak_vss_pages: u64,
}

impl ProcessVmStat {
    pub const fn new() -> Self {
        Self {
            vss_pages: AtomicI64::new(0),
            peak_vss_pages: AtomicU64::new(0),
        }
    }

    // ── Read accessors ────────────────────────────────────────────────────

    /// Current VSS in pages (VmSize).
    #[inline]
    pub fn vss_pages(&self) -> u64 {
        self.vss_pages.load(Ordering::Relaxed).max(0) as u64
    }

    /// Peak VSS in pages (VmPeak).
    #[inline]
    pub fn peak_vss_pages(&self) -> u64 {
        self.peak_vss_pages.load(Ordering::Relaxed)
    }

    pub(super) fn snapshot(&self) -> ProcessVmStatSnapshot {
        ProcessVmStatSnapshot {
            vss_pages: self.vss_pages.load(Ordering::Acquire),
            peak_vss_pages: self.peak_vss_pages.load(Ordering::Acquire),
        }
    }

    pub(super) fn restore(&self, snapshot: ProcessVmStatSnapshot) {
        self.vss_pages.store(snapshot.vss_pages, Ordering::Release);
        self.peak_vss_pages
            .store(snapshot.peak_vss_pages, Ordering::Release);
    }

    // ── Mutation (called only by AddrSpace) ───────────────────────────────

    /// Account for `pages` newly mapped pages and update high-water marks.
    ///
    /// Must be called **after** the mapping succeeds so that a failed map does
    /// not advance the watermarks.
    #[inline]
    pub(super) fn on_map(&self, pages: u64) {
        let new_vss = self
            .vss_pages
            .fetch_add(pages as i64, Ordering::Relaxed)
            .max(0) as u64
            + pages;
        self.peak_vss_pages.fetch_max(new_vss, Ordering::Relaxed);
    }

    /// Account for `pages` unmapped pages.  High-water marks are never lowered.
    #[inline]
    pub(super) fn on_unmap(&self, pages: u64) {
        self.vss_pages.fetch_sub(pages as i64, Ordering::Relaxed);
    }

    /// Reset all counters to zero (exec / address-space teardown).
    ///
    /// The virtual high-water mark is reset with the discarded image.
    /// Resident history is reset by the address space's separate owner.
    #[inline]
    pub(super) fn on_clear(&self) {
        self.vss_pages.store(0, Ordering::Relaxed);
        self.peak_vss_pages.store(0, Ordering::Relaxed);
    }

    /// Seed this stat from the VMAs actually copied into a fork child.
    ///
    /// The child inherits the parent's current VSS as its virtual watermark,
    /// matching Linux: the child's `mm_struct` starts with `hiwater_vm` set to
    /// the copied `total_vm`.
    #[inline]
    pub(super) fn seed_clone(&self, vss: u64) {
        self.vss_pages.store(vss as i64, Ordering::Relaxed);
        self.peak_vss_pages.store(vss, Ordering::Relaxed);
    }
}

impl Default for ProcessVmStat {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, not(axtest)))]
fn process_vm_stat_watermarks_hold_for_test() -> bool {
    let parent = ProcessVmStat::new();
    parent.on_map(3);
    parent.on_map(2);
    parent.on_unmap(4);

    let child = ProcessVmStat::new();
    child.seed_clone(parent.vss_pages());

    let inherited = child.vss_pages() == 1 && child.peak_vss_pages() == 1;
    parent.on_clear();

    inherited && parent.vss_pages() == 0 && parent.peak_vss_pages() == 0
}

#[cfg(all(test, not(axtest)))]
fn process_vm_stat_edge_cases_hold_for_test() -> bool {
    // Initial state: all zeros.
    let stat = ProcessVmStat::new();
    let init_ok = stat.vss_pages() == 0 && stat.peak_vss_pages() == 0;

    // Map advances VSS and peaks.
    stat.on_map(10);
    let after_map = stat.vss_pages() == 10 && stat.peak_vss_pages() == 10;

    // More mapping raises peaks further.
    stat.on_map(5);
    let after_more = stat.vss_pages() == 15 && stat.peak_vss_pages() == 15;

    // Unmap reduces VSS but peaks stay high.
    stat.on_unmap(8);
    let after_unmap = stat.vss_pages() == 7 && stat.peak_vss_pages() == 15;

    // Over-unmap: VSS goes signed but vss_pages() clamps to 0.
    stat.on_unmap(20); // 7 - 20 = -13, but .max(0) gives 0
    let after_over = stat.vss_pages() == 0; // clamped to 0
    // Peaks still at historical max.
    let peaks_stable = stat.peak_vss_pages() == 15;

    // seed_clone with an empty copied VMA set.
    let child = ProcessVmStat::new();
    child.seed_clone(0);
    let from_empty = child.vss_pages() == 0 && child.peak_vss_pages() == 0;

    init_ok && after_map && after_more && after_unmap && after_over && peaks_stable && from_empty
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn process_vm_stat_watermarks_hold() {
        assert!(super::process_vm_stat_watermarks_hold_for_test());
    }

    #[test]
    fn process_vm_stat_edge_cases_hold() {
        assert!(super::process_vm_stat_edge_cases_hold_for_test());
    }
}
