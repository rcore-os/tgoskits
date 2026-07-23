//! Per-address-space resident page counters (Linux `mm_struct.rss_stat` analogue).
//!
//! Counters use atomics for relaxed single-field updates; hiwater may lag
//! slightly under SMP (same as Linux). Mutations are expected under
//! the owning address-space lock or the current fault/populate path.

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoPreempt;
use ax_memory_addr::VirtAddr;
use log::warn;
use scope_local::scope_local;

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

/// Incremental RSS counters for one the address space.
///
/// COW-backend per-VA charges are serialized independently so every public
/// accounting operation remains safe even outside the address-space lock.
/// Charge-map operations run only in process context, never in IRQ handlers.
pub struct MemoryAccounting {
    rss_anon: AtomicU64,
    rss_file: AtomicU64,
    rss_shmem: AtomicU64,
    hiwater_rss: AtomicU64,
    /// Monotonic generation counter, incremented on every charge map mutation.
    generation: AtomicU64,
    /// Cow resident pages keyed by user VA (4 KiB granularity today).
    // The kernel allocator is non-sleeping, so BTreeMap insertion is valid in
    // this non-preemptible process-context critical section.
    charges: SpinNoPreempt<BTreeMap<VirtAddr, RssKind>>,
}

/// Validated RSS charge relocation performed as part of an `mremap` transaction.
pub struct PreparedChargeMoves {
    generation: u64,
    moves: alloc::vec::Vec<(VirtAddr, VirtAddr)>,
}

/// RSS charge relocation that can be reversed while the surrounding page-table
/// transaction is still fallible.
pub struct CommittedChargeMoves {
    moves: alloc::vec::Vec<(VirtAddr, VirtAddr)>,
}

impl PreparedChargeMoves {
    /// Applies the validated charge relocation.
    pub fn commit(self, accounting: &MemoryAccounting) -> AxResult<CommittedChargeMoves> {
        if accounting.generation.load(Ordering::Acquire) != self.generation {
            return Err(AxError::BadState);
        }

        let mut committed = alloc::vec::Vec::new();
        committed
            .try_reserve_exact(self.moves.len())
            .map_err(|_| AxError::NoMemory)?;
        for &(src, dst) in &self.moves {
            if let Err(error) = accounting.move_charge(src, dst) {
                for &(done_src, done_dst) in committed.iter().rev() {
                    if accounting.move_charge(done_dst, done_src).is_err() {
                        return Err(AxError::BadState);
                    }
                }
                return Err(error);
            }
            committed.push((src, dst));
        }
        Ok(CommittedChargeMoves { moves: committed })
    }
}

impl CommittedChargeMoves {
    /// Reverses the charge relocation after a later page-table operation fails.
    pub fn rollback(self, accounting: &MemoryAccounting) -> AxResult {
        for (src, dst) in self.moves.into_iter().rev() {
            accounting.move_charge(dst, src)?;
        }
        Ok(())
    }
}

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
            charges: SpinNoPreempt::new(BTreeMap::new()),
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

    pub fn charge_kind(&self, vaddr: VirtAddr) -> Option<RssKind> {
        self.charges.lock().get(&vaddr).copied()
    }

    /// Record a Cow resident page after PTE mapping succeeds.
    pub fn record_charge(&self, vaddr: VirtAddr, kind: RssKind) -> AxResult<()> {
        let mut charges = self.charges.lock();
        if charges.contains_key(&vaddr) {
            return Err(AxError::InvalidInput);
        }
        charges.insert(vaddr, kind);
        drop(charges);
        self.inc(kind, 1);
        self.generation.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Remove charge after PTE unmap. Debug builds assert the entry exists.
    pub fn remove_charge(&self, vaddr: VirtAddr) -> Option<RssKind> {
        let mut charges = self.charges.lock();
        let kind = charges.remove(&vaddr);
        drop(charges);
        match kind {
            Some(k) => {
                self.dec(k, 1);
                self.generation.fetch_add(1, Ordering::Relaxed);
                Some(k)
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
                if self.record_charge(vaddr, RssKind::Anon).is_err() {
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
    pub fn adopt_cow_write_as_anon(&self, vaddr: VirtAddr) -> AxResult<()> {
        self.record_charge(vaddr, RssKind::Anon)?;
        if self.rss_file_pages() > 0 {
            self.dec(RssKind::File, 1);
        }
        self.sync_rss_atomics_from_charges();
        Ok(())
    }

    /// Snapshot of all COW charge entries.
    pub fn charge_entries(&self) -> alloc::vec::Vec<(VirtAddr, RssKind)> {
        let charges = self.charges.lock();
        charges.iter().map(|(&va, &kind)| (va, kind)).collect()
    }

    /// Count resident COW charges by kind.
    pub fn snapshot_resident_charges(&self) -> (u64, u64, u64) {
        let charges = self.charges.lock();
        let mut anon = 0u64;
        let mut file = 0u64;
        let mut shmem = 0u64;
        for kind in charges.values() {
            match kind {
                RssKind::Anon => anon += 1,
                RssKind::File => file += 1,
                RssKind::Shmem => shmem += 1,
            }
        }
        (anon, file, shmem)
    }

    /// Fork: copy parent's bucket after child PTE maps the shared page.
    pub fn copy_charge_from(&self, parent: &Self, vaddr: VirtAddr) -> AxResult<()> {
        let kind = parent.charge_kind(vaddr).ok_or(AxError::InvalidInput)?;
        self.record_charge(vaddr, kind)?;
        Ok(())
    }

    /// After fork PTE setup, copy parent Cow charges that the backend clone operation
    /// missed for VAs mapped in the child page table.
    pub fn reconcile_fork_charges_from_parent(
        child: &Self,
        parent: &Self,
        mut is_child_page_mapped: impl FnMut(VirtAddr) -> AxResult<bool>,
    ) -> AxResult<()> {
        let parent_entries = parent.charge_entries();

        for (va, _) in &parent_entries {
            let Some(parent_kind) = parent.charge_kind(*va) else {
                continue;
            };
            if !is_child_page_mapped(*va)? {
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
            .map(|(va, _)| is_child_page_mapped(va).map(|mapped| (va, mapped)))
            .collect::<AxResult<alloc::vec::Vec<_>>>()?
            .into_iter()
            .filter_map(|(va, mapped)| (!mapped).then_some(va))
            .collect();
        for va in child_orphans {
            child.remove_charge(va);
        }

        child.sync_rss_atomics_from_charges();
        Ok(())
    }

    /// Validates all RSS charge moves before an `mremap` changes page tables.
    pub fn prepare_move_charges(
        &self,
        moves: impl IntoIterator<Item = (VirtAddr, VirtAddr)>,
    ) -> AxResult<PreparedChargeMoves> {
        let generation = self.generation.load(Ordering::Acquire);
        let charges = self.charges.lock();
        let mut prepared = alloc::vec::Vec::new();
        for (src, dst) in moves {
            if !charges.contains_key(&src) {
                continue;
            }
            if charges.contains_key(&dst)
                || prepared
                    .iter()
                    .any(|&(_, prepared_dst)| prepared_dst == dst)
            {
                return Err(AxError::InvalidInput);
            }
            prepared.try_reserve(1).map_err(|_| AxError::NoMemory)?;
            prepared.push((src, dst));
        }
        Ok(PreparedChargeMoves {
            generation,
            moves: prepared,
        })
    }

    /// mremap: migrate charge after PTE move (src unmapped, dst mapped).
    pub fn move_charge(&self, src: VirtAddr, dst: VirtAddr) -> AxResult<()> {
        let mut charges = self.charges.lock();
        let Some(kind) = charges.remove(&src) else {
            return Ok(());
        };
        if charges.contains_key(&dst) {
            debug_assert!(false, "move_charge: dst {dst:?} already charged");
            charges.insert(src, kind);
            return Err(AxError::InvalidInput);
        }
        charges.insert(dst, kind);
        self.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }
}

impl Drop for MemoryAccounting {
    fn drop(&mut self) {
        let charges = self.charges.get_mut();
        let kinds: alloc::vec::Vec<_> = charges.values().copied().collect();
        charges.clear();
        for kind in kinds {
            self.dec(kind, 1);
        }
    }
}

/// Parent/child RSS handles passed through the backend clone operation.
pub struct CloneMapAccounting<'a> {
    pub parent: Option<&'a MemoryAccounting>,
    pub child: Option<&'a MemoryAccounting>,
}

/// Guard that publishes `acct` to [`MappingBackend`](ax_memory_set::MappingBackend) bridge calls.
pub struct RssAccountingGuard<'a> {
    prev: usize,
    _accounting: core::marker::PhantomData<&'a MemoryAccounting>,
    _not_send: core::marker::PhantomData<alloc::rc::Rc<()>>,
}

impl<'a> RssAccountingGuard<'a> {
    pub fn enter(acct: &'a MemoryAccounting) -> Self {
        let prev = RSS_ACCOUNTING.with(|current| {
            current.swap(acct as *const MemoryAccounting as usize, Ordering::Relaxed)
        });
        Self {
            prev,
            _accounting: core::marker::PhantomData,
            _not_send: core::marker::PhantomData,
        }
    }
}

impl Drop for RssAccountingGuard<'_> {
    fn drop(&mut self) {
        RSS_ACCOUNTING.with(|current| current.store(self.prev, Ordering::Relaxed));
    }
}

/// Runs a backend bridge operation with the accounting object published by the
/// current [`RssAccountingGuard`]. The reference cannot escape the callback.
#[doc(hidden)]
pub fn with_rss_accounting<R>(operation: impl FnOnce(Option<&MemoryAccounting>) -> R) -> R {
    let ptr = RSS_ACCOUNTING.with(|current| current.load(Ordering::Relaxed));
    if ptr == 0 {
        operation(None)
    } else {
        // SAFETY: `RssAccountingGuard<'a>` publishes a pointer derived from an
        // `&'a MemoryAccounting`, cannot move to another execution context, and
        // restores the prior pointer before `'a` ends. The reference is passed
        // only to this callback and therefore cannot acquire a wider lifetime.
        operation(Some(unsafe { &*(ptr as *const MemoryAccounting) }))
    }
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
        acct.record_charge(va, RssKind::File).unwrap();
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
        acct.record_charge(src, RssKind::File).unwrap();
        acct.move_charge(src, dst).unwrap();
        assert!(acct.charge_kind(src).is_none());
        assert_eq!(acct.charge_kind(dst), Some(RssKind::File));
        assert_eq!(acct.rss_file_pages(), 1);
    }

    #[test]
    fn prepared_charge_moves_roll_back_as_one_transaction() {
        let accounting = MemoryAccounting::new();
        let first = VirtAddr::from(0x1000);
        let second = VirtAddr::from(0x2000);
        let first_dst = VirtAddr::from(0x5000);
        let second_dst = VirtAddr::from(0x6000);
        accounting.record_charge(first, RssKind::Anon).unwrap();
        accounting.record_charge(second, RssKind::File).unwrap();

        let prepared = accounting
            .prepare_move_charges([(first, first_dst), (second, second_dst)])
            .unwrap();
        let committed = prepared.commit(&accounting).unwrap();
        assert_eq!(accounting.charge_kind(first_dst), Some(RssKind::Anon));
        assert_eq!(accounting.charge_kind(second_dst), Some(RssKind::File));

        committed.rollback(&accounting).unwrap();
        assert_eq!(accounting.charge_kind(first), Some(RssKind::Anon));
        assert_eq!(accounting.charge_kind(second), Some(RssKind::File));
        assert_eq!(accounting.charge_kind(first_dst), None);
        assert_eq!(accounting.charge_kind(second_dst), None);
    }

    #[test]
    fn prepared_charge_moves_reject_occupied_destinations_without_mutation() {
        let accounting = MemoryAccounting::new();
        let src = VirtAddr::from(0x1000);
        let dst = VirtAddr::from(0x5000);
        accounting.record_charge(src, RssKind::Anon).unwrap();
        accounting.record_charge(dst, RssKind::File).unwrap();

        assert!(accounting.prepare_move_charges([(src, dst)]).is_err());
        assert_eq!(accounting.charge_kind(src), Some(RssKind::Anon));
        assert_eq!(accounting.charge_kind(dst), Some(RssKind::File));
    }

    #[test]
    fn copy_charge_from_parent() {
        let parent = MemoryAccounting::new();
        let child = MemoryAccounting::new();
        let va = VirtAddr::from(0x3000usize);
        parent.record_charge(va, RssKind::File).unwrap();
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
            parent.record_charge(va, kind).unwrap();
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
        parent.record_charge(va, RssKind::File).unwrap();
        child.copy_charge_from(&parent, va).unwrap();
        let (parent_anon, ..) = parent.snapshot_resident_charges();
        assert!(child.cow_file_write_to_anon(va));
        let (child_anon, child_file, _) = child.snapshot_resident_charges();
        assert_eq!(child_anon, parent_anon + 1);
        assert_eq!(child_file, 0);
    }

    #[test]
    fn accounting_bridge_is_scoped_to_the_guard() {
        let area_count = core::num::NonZeroU32::new(1).unwrap();
        ax_percpu::host_test::initialize(area_count).unwrap();
        let cpu_index = ax_percpu::CpuIndex::try_from(0).unwrap();
        let area = ax_percpu::area(cpu_index).unwrap();
        // SAFETY: this test thread models CPU 0, installs its initialized area
        // once, and never changes the modeled CPU for the rest of the process.
        unsafe { cpu_local::install_cpu_area(area.cpu_area().unwrap()) }.unwrap();

        let acct = MemoryAccounting::new();
        with_rss_accounting(|current| assert!(current.is_none()));
        {
            let _guard = RssAccountingGuard::enter(&acct);
            with_rss_accounting(|current| {
                assert!(core::ptr::eq(current.unwrap(), &acct));
            });
        }
        with_rss_accounting(|current| assert!(current.is_none()));
    }
}
