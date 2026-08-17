//! Per-address-space resident page counters (Linux `mm_struct.rss_stat` analogue).
//!
//! Counters use atomics for relaxed single-field updates; hiwater may lag
//! slightly under SMP (same as Linux). Mutations are expected under
//! [`super::AddrSpace`] lock or the current fault/populate path.

use alloc::collections::BTreeMap;
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use ax_memory_addr::VirtAddr;
use scope_local::scope_local;

use crate::{StarryError, StarryResult};

static GENERATION_ID: AtomicU64 = AtomicU64::new(1);

scope_local! {
    static RSS_ACCOUNTING: AtomicUsize = AtomicUsize::new(0);
}

/// Resident page category matching Linux `MM_*PAGES` buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RssKind {
    Anon,
    File,
    Shmem,
}

/// One Cow charge entry: its kind and the number of 4 KiB resident pages it
/// accounts for. Almost always 1 (a 4 KiB page); a transparent-huge-page 2 MiB
/// block is a single entry accounting for its 512 resident pages, so VmRSS/VmHWM
/// reflect the whole block rather than one page.
#[derive(Debug, Clone, Copy)]
struct RssCharge {
    kind: RssKind,
    pages: u32,
}

/// Incremental RSS counters for one [`super::AddrSpace`].
///
/// Cow-backend per-VA charges live in `charges` and are mutated only while the
/// owning [`super::AddrSpace`] mutex is held.
pub struct MemoryAccounting {
    rss_anon: AtomicU64,
    rss_file: AtomicU64,
    rss_shmem: AtomicU64,
    hiwater_rss: AtomicU64,
    /// Monotonic generation counter, incremented on every charge map mutation.
    generation: AtomicU64,
    /// Cow resident charges keyed by user VA. Each entry accounts for its
    /// [`RssCharge::pages`] resident 4 KiB pages (1 for a 4 KiB page, 512 for a THP
    /// 2 MiB block).
    charges: UnsafeCell<BTreeMap<VirtAddr, RssCharge>>,
}

// SAFETY: `charges` is only accessed while `AddrSpace` is locked.
unsafe impl Sync for MemoryAccounting {}

impl Default for MemoryAccounting {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryAccounting {
    pub fn new() -> Self {
        let gen_id = GENERATION_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            rss_anon: AtomicU64::new(0),
            rss_file: AtomicU64::new(0),
            rss_shmem: AtomicU64::new(0),
            hiwater_rss: AtomicU64::new(0),
            generation: AtomicU64::new(gen_id),
            charges: UnsafeCell::new(BTreeMap::new()),
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
        self.rss_anon_pages() + self.rss_file_pages() + self.rss_shmem_pages()
    }

    /// Linux `get_mm_hiwater_rss`: max(stored peak, current total).
    pub fn hiwater_rss_pages(&self) -> u64 {
        self.hiwater_rss
            .load(Ordering::Relaxed)
            .max(self.rss_total_pages())
    }

    fn counter(&self, kind: RssKind) -> &AtomicU64 {
        match kind {
            RssKind::Anon => &self.rss_anon,
            RssKind::File => &self.rss_file,
            RssKind::Shmem => &self.rss_shmem,
        }
    }

    fn update_hiwater(&self, total: u64) {
        let mut hw = self.hiwater_rss.load(Ordering::Relaxed);
        while total > hw {
            match self.hiwater_rss.compare_exchange_weak(
                hw,
                total,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => hw = x,
            }
        }
    }

    pub fn inc(&self, kind: RssKind, pages: u64) {
        if pages == 0 {
            return;
        }
        self.counter(kind).fetch_add(pages, Ordering::Relaxed);
        self.update_hiwater(self.rss_total_pages());
    }

    pub fn dec(&self, kind: RssKind, pages: u64) {
        if pages == 0 {
            return;
        }
        let prev = self.counter(kind).fetch_sub(pages, Ordering::Relaxed);
        debug_assert!(prev >= pages, "RSS {kind:?} underflow");
    }

    pub(crate) fn charge_kind(&self, vaddr: VirtAddr) -> Option<RssKind> {
        // SAFETY: `AddrSpace` lock held by all callers.
        let charges = unsafe { &*self.charges.get() };
        charges.get(&vaddr).map(|c| c.kind)
    }

    /// The full charge entry (kind + resident page count) at `vaddr`, if any.
    fn charge_at(&self, vaddr: VirtAddr) -> Option<RssCharge> {
        // SAFETY: `AddrSpace` lock held by all callers.
        let charges = unsafe { &*self.charges.get() };
        charges.get(&vaddr).copied()
    }

    /// Record a Cow resident charge of `pages` 4 KiB pages after PTE mapping
    /// succeeds. `pages` is 1 for a 4 KiB page, 512 for a THP 2 MiB block.
    pub fn record_charge(&self, vaddr: VirtAddr, kind: RssKind, pages: u32) -> StarryResult<()> {
        // SAFETY: `AddrSpace` lock held by all callers.
        let charges = unsafe { &mut *self.charges.get() };
        if charges.contains_key(&vaddr) {
            return Err(StarryError::InvalidInput);
        }
        charges.insert(vaddr, RssCharge { kind, pages });
        self.inc(kind, pages as u64);
        self.generation.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Infallible per-VA charge insert for a freshly-uncharged page. Used by a THP
    /// 2 MiB -> 4 KiB split to expand one 2 MiB charge into 512 4 KiB charges: the
    /// sub-page VAs were never charged (only the 2 MiB VA was), so the insert cannot
    /// collide and must not fail partway through the 512. Debug builds assert the
    /// freshness invariant.
    #[cfg(feature = "thp")]
    pub fn record_charge_fresh(&self, vaddr: VirtAddr, kind: RssKind) {
        // SAFETY: `AddrSpace` lock held by all callers.
        let charges = unsafe { &mut *self.charges.get() };
        debug_assert!(
            !charges.contains_key(&vaddr),
            "record_charge_fresh: duplicate charge for {vaddr:?}"
        );
        charges.insert(vaddr, RssCharge { kind, pages: 1 });
        self.inc(kind, 1);
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Remove the charge after PTE unmap, decrementing by its resident page count
    /// (512 for a THP 2 MiB block). Debug builds assert the entry exists.
    pub fn remove_charge(&self, vaddr: VirtAddr) -> Option<RssKind> {
        // SAFETY: `AddrSpace` lock held by all callers.
        let charges = unsafe { &mut *self.charges.get() };
        match charges.remove(&vaddr) {
            Some(c) => {
                self.dec(c.kind, c.pages as u64);
                self.generation.fetch_add(1, Ordering::Relaxed);
                Some(c.kind)
            }
            None => {
                debug_assert!(false, "remove_charge: missing entry for {vaddr:?}");
                warn!("remove_charge: missing entry for {vaddr:?}");
                None
            }
        }
    }

    /// Reconcile relaxed atomic buckets with the Cow charge map.
    ///
    /// File-backend pages only bump atomics (no charge entry). The gap between
    /// atomic and charged file pages is preserved across sync.
    pub fn sync_rss_atomics_from_charges(&self) {
        let (ca, cf, cs) = self.snapshot_resident_charges();
        let af = self.rss_file.load(Ordering::Relaxed);
        let ash = self.rss_shmem.load(Ordering::Relaxed);
        let file_only = af.saturating_sub(cf);
        let shmem_only = ash.saturating_sub(cs);
        self.rss_anon.store(ca, Ordering::Release);
        self.rss_file
            .store(cf.saturating_add(file_only), Ordering::Release);
        self.rss_shmem
            .store(cs.saturating_add(shmem_only), Ordering::Release);
        self.update_hiwater(self.rss_total_pages());
    }

    /// File→Anon for a file-backed MAP_PRIVATE COW write fault.
    ///
    /// Uses explicit remove + record so the charge map and atomics stay aligned.
    pub fn cow_file_write_to_anon(&self, vaddr: VirtAddr) -> bool {
        let kind = self.charge_kind(vaddr);
        match kind {
            Some(RssKind::Anon) => {
                self.sync_rss_atomics_from_charges();
                true
            }
            Some(RssKind::File) => {
                self.remove_charge(vaddr);
                if self.record_charge(vaddr, RssKind::Anon, 1).is_err() {
                    return false;
                }
                self.sync_rss_atomics_from_charges();
                true
            }
            Some(RssKind::Shmem) => false,
            None => match self.adopt_cow_write_as_anon(vaddr) {
                Ok(()) => true,
                Err(_) => false,
            },
        }
    }

    /// Establish an Anon charge after a file-backed COW write when no File
    /// charge exists at `vaddr` (accounting drift recovery).
    pub fn adopt_cow_write_as_anon(&self, vaddr: VirtAddr) -> StarryResult<()> {
        self.record_charge(vaddr, RssKind::Anon, 1)?;
        if self.rss_file_pages() > 0 {
            self.dec(RssKind::File, 1);
        }
        self.sync_rss_atomics_from_charges();
        Ok(())
    }

    /// Snapshot of all Cow charge entries. Only valid while [`super::AddrSpace`] is locked.
    pub fn charge_entries(&self) -> alloc::vec::Vec<(VirtAddr, RssKind)> {
        // SAFETY: `AddrSpace` lock held by all callers.
        let charges = unsafe { &*self.charges.get() };
        charges.iter().map(|(&va, c)| (va, c.kind)).collect()
    }

    /// Count resident Cow pages by kind (a THP block counts its 512 pages). Only
    /// valid while [`super::AddrSpace`] is locked.
    pub fn snapshot_resident_charges(&self) -> (u64, u64, u64) {
        // SAFETY: `AddrSpace` lock held by all callers.
        let charges = unsafe { &*self.charges.get() };
        let mut anon = 0u64;
        let mut file = 0u64;
        let mut shmem = 0u64;
        for c in charges.values() {
            let pages = c.pages as u64;
            match c.kind {
                RssKind::Anon => anon += pages,
                RssKind::File => file += pages,
                RssKind::Shmem => shmem += pages,
            }
        }
        (anon, file, shmem)
    }

    /// Fork: copy parent's bucket after child PTE maps the shared page, preserving
    /// the page count (a shared THP block stays a 512-page charge in the child).
    pub fn copy_charge_from(&self, parent: &Self, vaddr: VirtAddr) -> StarryResult<()> {
        let charge = parent.charge_at(vaddr).ok_or(StarryError::InvalidInput)?;
        self.record_charge(vaddr, charge.kind, charge.pages)?;
        Ok(())
    }

    /// After fork PTE setup, copy parent Cow charges that [`super::BackendOps::clone_map`]
    /// missed for VAs mapped in the child page table.
    pub fn reconcile_fork_charges_from_parent(
        child: &Self,
        parent: &Self,
        child_pt: &mut ax_runtime::hal::paging::PageTable,
    ) -> StarryResult<()> {
        use ax_runtime::hal::paging::PagingError;

        let parent_entries = parent.charge_entries();

        for (va, _) in &parent_entries {
            let Some(parent_kind) = parent.charge_kind(*va) else {
                continue;
            };
            if child_pt.query(*va).is_err() {
                continue;
            }
            match child.charge_kind(*va) {
                None => {
                    child.copy_charge_from(parent, *va)?;
                }
                Some(child_kind) if child_kind != parent_kind => {
                    child.remove_charge(*va);
                    child.copy_charge_from(parent, *va)?;
                }
                _ => {}
            }
        }

        let child_orphans: alloc::vec::Vec<_> = child
            .charge_entries()
            .into_iter()
            .filter(|(va, _)| child_pt.query(*va) == Err(PagingError::NotMapped))
            .map(|(va, _)| va)
            .collect();
        for va in child_orphans {
            child.remove_charge(va);
        }

        child.sync_rss_atomics_from_charges();
        Ok(())
    }

    /// mremap: migrate charge after PTE move (src unmapped, dst mapped).
    pub fn move_charge(&self, src: VirtAddr, dst: VirtAddr) -> StarryResult<()> {
        // SAFETY: `AddrSpace` lock held by all callers.
        let charges = unsafe { &mut *self.charges.get() };
        let Some(charge) = charges.remove(&src) else {
            return Ok(());
        };
        if charges.contains_key(&dst) {
            debug_assert!(false, "move_charge: dst {dst:?} already charged");
            charges.insert(src, charge);
            return Err(StarryError::InvalidInput);
        }
        charges.insert(dst, charge);
        Ok(())
    }
}

impl Drop for MemoryAccounting {
    fn drop(&mut self) {
        let charges = self.charges.get_mut();
        let entries: alloc::vec::Vec<_> = charges.values().copied().collect();
        charges.clear();
        for c in entries {
            self.dec(c.kind, c.pages as u64);
        }
    }
}

/// Parent/child RSS handles passed through [`super::backend::BackendOps::clone_map`].
pub struct CloneMapAccounting<'a> {
    pub parent: Option<&'a MemoryAccounting>,
    pub child: Option<&'a MemoryAccounting>,
}

/// Guard that publishes `acct` to [`MappingBackend`](ax_memory_set::MappingBackend) bridge calls.
pub struct RssAccountingGuard<'a> {
    prev: usize,
    _not_send: core::marker::PhantomData<&'a ()>,
}

impl<'a> RssAccountingGuard<'a> {
    pub fn enter(acct: &'a MemoryAccounting) -> Self {
        let prev = RSS_ACCOUNTING.with(|current| {
            current.swap(acct as *const MemoryAccounting as usize, Ordering::Relaxed)
        });
        Self {
            prev,
            _not_send: core::marker::PhantomData,
        }
    }
}

impl Drop for RssAccountingGuard<'_> {
    fn drop(&mut self) {
        RSS_ACCOUNTING.with(|current| current.store(self.prev, Ordering::Relaxed));
    }
}

/// Used only from `backend/mod.rs` `MappingBackend` bridge.
pub(crate) fn bridge_rss_accounting() -> Option<&'static MemoryAccounting> {
    let ptr = RSS_ACCOUNTING.with(|current| current.load(Ordering::Relaxed));
    if ptr == 0 {
        None
    } else {
        Some(unsafe { &*(ptr as *const MemoryAccounting) })
    }
}

#[cfg(axtest)]
pub(crate) fn accounting_edge_cases_and_snapshot_rules_hold_for_test() -> bool {
    use ax_memory_addr::VirtAddr;

    // inc(0) and dec(0) are no-ops.
    let acct = MemoryAccounting::new();
    acct.inc(RssKind::Anon, 0);
    assert!(acct.rss_total_pages() == 0);
    acct.dec(RssKind::File, 0);
    assert!(acct.rss_total_pages() == 0);

    // hiwater_rss starts at 0 and tracks max(total, stored).
    assert!(acct.hiwater_rss_pages() == 0);
    acct.inc(RssKind::Anon, 5);
    assert!(acct.hiwater_rss_pages() == 5);
    acct.dec(RssKind::Anon, 3);
    // hiwater stays at peak even after dec.
    assert!(acct.hiwater_rss_pages() == 5);
    assert!(acct.rss_total_pages() == 2);

    // snapshot_resident_charges counts by kind from charge map.
    let va1 = VirtAddr::from(0x1000usize);
    let va2 = VirtAddr::from(0x2000usize);
    let va3 = VirtAddr::from(0x3000usize);
    acct.record_charge(va1, RssKind::Anon, 1).unwrap();
    acct.record_charge(va2, RssKind::File, 1).unwrap();
    acct.record_charge(va3, RssKind::File, 1).unwrap();
    let (anon, file, shmem) = acct.snapshot_resident_charges();
    assert_eq!(anon, 1); // va1
    assert_eq!(file, 2); // va2 + va3
    assert_eq!(shmem, 0);

    // generation is monotonic (new() sets it from global counter).
    let gen1 = acct.generation.load(core::sync::atomic::Ordering::Relaxed);
    acct.remove_charge(va1);
    let gen2 = acct.generation.load(core::sync::atomic::Ordering::Relaxed);
    assert!(gen2 > gen1);

    // move_charge with non-existent src is a no-op (returns Ok).
    let ghost = VirtAddr::from(0xDEADusize);
    assert!(acct.move_charge(ghost, VirtAddr::from(0xBEEFusize)).is_ok());
    assert!(acct.charge_kind(ghost).is_none());

    // A transparent-huge-page 2 MiB block is a single charge entry that accounts
    // for its 512 resident 4 KiB pages, so VmRSS/VmHWM reflect the whole block.
    // Regression: a resident 2 MiB block previously counted as 1 page, so touching
    // a THP-promoted mmap under-grew VmHWM (`proc-test-vmpeak-hwm`). This asserts
    // the block charge counts, peaks, and decrements as 512 pages.
    let huge = MemoryAccounting::new();
    let hva = VirtAddr::from(0x20_0000usize);
    huge.record_charge(hva, RssKind::Anon, 512).unwrap();
    assert_eq!(huge.rss_anon_pages(), 512);
    assert_eq!(huge.rss_total_pages(), 512);
    assert_eq!(huge.hiwater_rss_pages(), 512);
    let (ha, hf, hs) = huge.snapshot_resident_charges();
    assert_eq!(ha, 512);
    assert_eq!(hf, 0);
    assert_eq!(hs, 0);
    huge.remove_charge(hva);
    assert_eq!(huge.rss_total_pages(), 0);
    // hiwater keeps the 512 peak even after the block is unmapped.
    assert_eq!(huge.hiwater_rss_pages(), 512);

    // 2 MiB -> 4 KiB split conservation: replacing one 512-page block charge with
    // 512 single-page charges leaves VmRSS unchanged (-512 + 512 * 1 == 0).
    let split = MemoryAccounting::new();
    let base = VirtAddr::from(0x40_0000usize);
    split.record_charge(base, RssKind::Anon, 512).unwrap();
    assert_eq!(split.rss_total_pages(), 512);
    split.remove_charge(base);
    for i in 0..512usize {
        split
            .record_charge(base + i * 0x1000, RssKind::Anon, 1)
            .unwrap();
    }
    assert_eq!(split.rss_total_pages(), 512);

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inc_dec_and_hiwater() {
        let acct = MemoryAccounting::new();
        acct.inc(RssKind::Anon, 4);
        assert_eq!(acct.rss_total_pages(), 4);
        assert_eq!(acct.hiwater_rss_pages(), 4);
        acct.inc(RssKind::File, 2);
        assert_eq!(acct.rss_total_pages(), 6);
        acct.dec(RssKind::Anon, 1);
        assert_eq!(acct.rss_anon_pages(), 3);
        assert_eq!(acct.hiwater_rss_pages(), 6);
    }

    #[test]
    fn record_remove_and_reclassify() {
        let acct = MemoryAccounting::new();
        let va = VirtAddr::from(0x1000usize);
        acct.record_charge(va, RssKind::File, 1).unwrap();
        assert_eq!(acct.rss_file_pages(), 1);
        assert!(acct.cow_file_write_to_anon(va));
        assert_eq!(acct.rss_file_pages(), 0);
        assert_eq!(acct.rss_anon_pages(), 1);
        acct.remove_charge(va);
        assert_eq!(acct.rss_total_pages(), 0);
    }

    #[test]
    fn move_charge() {
        let acct = MemoryAccounting::new();
        let src = VirtAddr::from(0x1000usize);
        let dst = VirtAddr::from(0x2000usize);
        acct.record_charge(src, RssKind::File, 1).unwrap();
        acct.move_charge(src, dst).unwrap();
        assert!(acct.charge_kind(src).is_none());
        assert_eq!(acct.charge_kind(dst), Some(RssKind::File));
        assert_eq!(acct.rss_file_pages(), 1);
    }

    #[test]
    fn copy_charge_from_parent() {
        let parent = MemoryAccounting::new();
        let child = MemoryAccounting::new();
        let va = VirtAddr::from(0x3000usize);
        parent.record_charge(va, RssKind::File, 1).unwrap();
        child.copy_charge_from(&parent, va).unwrap();
        assert_eq!(parent.rss_file_pages(), 1);
        assert_eq!(child.rss_file_pages(), 1);
    }

    #[test]
    fn fork_charge_parity_after_copy() {
        let parent = MemoryAccounting::new();
        let child = MemoryAccounting::new();
        let pages = [
            (VirtAddr::from(0x1000usize), RssKind::File),
            (VirtAddr::from(0x2000usize), RssKind::Anon),
            (VirtAddr::from(0x9000usize), RssKind::File),
        ];
        for (va, kind) in pages {
            parent.record_charge(va, kind, 1).unwrap();
        }
        for (va, _) in pages {
            child.copy_charge_from(&parent, va).unwrap();
        }
        assert_eq!(
            parent.snapshot_resident_charges(),
            child.snapshot_resident_charges()
        );
        parent.sync_rss_atomics_from_charges();
        child.sync_rss_atomics_from_charges();
        assert_eq!(parent.rss_anon_pages(), child.rss_anon_pages());
        assert_eq!(parent.rss_file_pages(), child.rss_file_pages());
    }

    #[test]
    fn cow_write_after_fork_parity_increments_anon() {
        let parent = MemoryAccounting::new();
        let child = MemoryAccounting::new();
        let va = VirtAddr::from(0x9000usize);
        parent.record_charge(va, RssKind::File, 1).unwrap();
        child.copy_charge_from(&parent, va).unwrap();
        let (parent_anon, ..) = parent.snapshot_resident_charges();
        assert!(child.cow_file_write_to_anon(va));
        let (child_anon, child_file, _) = child.snapshot_resident_charges();
        assert_eq!(child_anon, parent_anon + 1);
        assert_eq!(child_file, 0);
    }
}

#[cfg(axtest)]
pub(crate) fn rss_kind_and_accounting_rules_hold_for_test() -> bool {
    // RssKind variants are Debug, Clone, Copy, PartialEq, Eq
    let anon = RssKind::Anon;
    let file = RssKind::File;
    let shmem = RssKind::Shmem;

    // PartialEq
    assert!(anon == RssKind::Anon);
    assert!(anon != file);
    assert!(file != shmem);

    // Clone
    let anon2 = anon.clone();
    assert!(anon2 == anon);

    // Default MemoryAccounting has zero counters
    let acc = MemoryAccounting::new();
    let (a, f, s) = acc.snapshot_resident_charges();
    assert!(a == 0);
    assert!(f == 0);
    assert!(s == 0);

    true
}

#[cfg(axtest)]
pub(crate) fn accounting_rss_kind_debug_and_default_hold_for_test() -> bool {
    // Test MemoryAccounting default trait
    let acc_default = MemoryAccounting::default();
    assert_eq!(acc_default.rss_anon_pages(), 0);
    assert_eq!(acc_default.rss_file_pages(), 0);
    assert_eq!(acc_default.rss_shmem_pages(), 0);
    assert_eq!(acc_default.rss_total_pages(), 0);

    // Test individual rss getters
    let acc_new = MemoryAccounting::new();
    assert_eq!(acc_new.rss_anon_pages(), 0);
    assert_eq!(acc_new.rss_file_pages(), 0);
    assert_eq!(acc_new.rss_shmem_pages(), 0);

    // Test RssKind Copy trait
    let anon = RssKind::Anon;
    let copied = anon;
    assert_eq!(anon, copied);

    true
}
