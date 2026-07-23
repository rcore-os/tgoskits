//! Per-address-space virtual memory accounting (VmX).
//!
//! [`ProcessVmStat`] is the single authoritative source for all VmX counters.
//! It lives inside the owning address space and is maintained automatically by
//! `map` / `unmap` / `clear` / `try_clone`, so no syscall handler needs to
//! touch it manually.
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

    // ── Mutation (called only by AddrSpace) ───────────────────────────────

    /// Account for `pages` newly mapped pages and update high-water marks.
    ///
    /// Must be called **after** the mapping succeeds so that a failed map does
    /// not advance the watermarks.
    #[inline]
    pub fn on_map(&self, pages: u64) {
        let new_vss = self
            .vss_pages
            .fetch_add(pages as i64, Ordering::Relaxed)
            .max(0) as u64
            + pages;
        self.peak_vss_pages.fetch_max(new_vss, Ordering::Relaxed);
    }

    /// Account for `pages` unmapped pages.  High-water marks are never lowered.
    #[inline]
    pub fn on_unmap(&self, pages: u64) {
        self.vss_pages.fetch_sub(pages as i64, Ordering::Relaxed);
    }

    /// Atomically accounts for a VMA replacement after it commits.
    #[inline]
    pub fn on_replace(&self, removed_pages: u64, added_pages: u64) {
        let delta = added_pages as i64 - removed_pages as i64;
        let previous = self.vss_pages.fetch_add(delta, Ordering::Relaxed);
        let new_vss = previous.saturating_add(delta).max(0) as u64;
        self.peak_vss_pages.fetch_max(new_vss, Ordering::Relaxed);
    }

    /// Reset all counters to zero (exec / address-space teardown).
    ///
    /// High-water marks are also reset: Linux resets VmPeak/VmHWM on `execve`
    /// because the new image starts a fresh `mm_struct`.
    #[inline]
    pub fn on_clear(&self) {
        self.vss_pages.store(0, Ordering::Relaxed);
        self.peak_vss_pages.store(0, Ordering::Relaxed);
    }

    /// Seed this stat from a parent's snapshot (used when `try_clone` builds
    /// the child address space for `fork`/`clone`).
    ///
    /// The child inherits the parent's current VSS as its starting watermarks,
    /// matching Linux: the child's `mm_struct` starts with `hiwater_vm` set to
    /// the copied `total_vm`.
    #[inline]
    pub fn seed_from(&self, parent: &Self) {
        let vss = parent.vss_pages();
        self.vss_pages.store(vss as i64, Ordering::Relaxed);
        // Child's peaks start at current VSS (the copied address space size).
        self.peak_vss_pages
            .store(parent.peak_vss_pages().max(vss), Ordering::Relaxed);
    }
}

impl Default for ProcessVmStat {
    fn default() -> Self {
        Self::new()
    }
}
