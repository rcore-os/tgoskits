use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
};
use core::slice;

use ax_errno::{AxError, AxResult};
use ax_fs_ng::vfs::FileBackend;
use ax_kspin::SpinNoIrq;
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange};
#[cfg(axtest)]
use ax_runtime::hal::paging::PageTable;
use ax_runtime::hal::{
    mem::phys_to_virt,
    paging::{MappingFlags, PageSize, PageTableCursor, PagingError},
};
use ax_sync::Mutex;
use starry_mm::{
    CowFrameReferences, CowRelease, PageInitialization, PrivateFileMapping, VmFile, VmFileInfo,
};

use super::{
    AddrSpace, Backend, BackendFileInfo, BackendOps, CloneMapAccounting, MemoryAccounting,
    PopulateCallback, RssKind, alloc_frame, dealloc_frame, pages_in,
};

type FrameRefCnt = CowFrameReferences;

struct KernelVmFile(ax_fs_ng::vfs::FileBackend);

impl VmFile for KernelVmFile {
    fn size_bytes(&self) -> AxResult<u64> {
        self.0.len()
    }

    fn read_at(&self, buffer: &mut [u8], offset: u64) -> AxResult<usize> {
        self.0.read_at(&mut &mut *buffer, offset)
    }

    fn info(&self) -> AxResult<VmFileInfo> {
        let location = self.0.location();
        Ok(VmFileInfo {
            path: location.absolute_path()?.to_string(),
            inode: location.inode(),
            device: location.metadata()?.device,
        })
    }
}

fn release_frame(reference: &mut FrameRefCnt, paddr: PhysAddr, page_size: PageSize) {
    if reference.release() == CowRelease::LastReference {
        FRAME_TABLE.lock().remove_frame(paddr);
        dealloc_frame(paddr, page_size);
    }
}

pub(super) fn retain_frame_for_transaction(paddr: PhysAddr) -> AxResult<()> {
    let frame = FRAME_TABLE
        .lock()
        .get_frame_ref(paddr)
        .ok_or(AxError::BadAddress)?;
    frame.lock().try_add().map_err(|_| AxError::BadState)
}

pub(super) fn release_transaction_frame(paddr: PhysAddr, page_size: PageSize) {
    let frame = FRAME_TABLE
        .lock()
        .get_frame_ref(paddr)
        .expect("transaction-held COW frame must remain registered");
    release_frame(&mut frame.lock(), paddr, page_size);
}

struct FrameTableRefCount {
    table: BTreeMap<PhysAddr, Arc<SpinNoIrq<FrameRefCnt>>>,
}

impl FrameTableRefCount {
    const fn new() -> Self {
        Self {
            table: BTreeMap::new(),
        }
    }

    fn get_frame_ref(&self, paddr: PhysAddr) -> Option<Arc<SpinNoIrq<FrameRefCnt>>> {
        self.table.get(&paddr).cloned()
    }

    fn init_frame(&mut self, paddr: PhysAddr) {
        assert!(
            !self.table.contains_key(&paddr),
            "initializing already referenced frame"
        );
        self.table
            .insert(paddr, Arc::new(SpinNoIrq::new(FrameRefCnt::new())));
    }

    fn remove_frame(&mut self, paddr: PhysAddr) {
        assert!(
            self.table.contains_key(&paddr),
            "removing unreferenced frame"
        );
        self.table.remove(&paddr);
    }
}

static FRAME_TABLE: SpinNoIrq<FrameTableRefCount> = SpinNoIrq::new(FrameTableRefCount::new());

#[cfg(axtest)]
pub(crate) fn private_mmap_eof_check_for_test() -> bool {
    starry_mm::private_file_eof_policy_matches_linux_for_test()
}

#[cfg(axtest)]
pub(crate) fn fault_accounting_failure_rolls_back_for_test() -> bool {
    let Ok(mut page_table) = PageTable::try_new() else {
        return false;
    };
    let vaddr = VirtAddr::from(0x20_0000);
    let backend = CowBackend {
        start: vaddr,
        size: PageSize::Size4K,
        file: None,
        name: None,
        shared: false,
    };
    let accounting = MemoryAccounting::new();
    let duplicate_charge = vaddr + PAGE_SIZE_4K;
    if accounting
        .record_charge(duplicate_charge, RssKind::Anon)
        .is_err()
    {
        return false;
    }

    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let result = backend.alloc_file_run(
        &[vaddr, duplicate_charge],
        flags,
        MappingFlags::READ,
        Some(&accounting),
        &mut page_table.cursor(),
    );
    let mut mappings_removed = true;
    for address in [vaddr, duplicate_charge] {
        let query = page_table.cursor().query(address);
        if let Ok((paddr, _, page_size)) = query {
            mappings_removed = false;
            let _ = page_table.cursor().unmap(address);
            let frame = FRAME_TABLE.lock().get_frame_ref(paddr);
            if let Some(frame) = frame {
                release_frame(&mut frame.lock(), paddr, page_size);
            }
        }
    }
    let accounting_rolled_back = accounting.charge_kind(vaddr).is_none()
        && accounting.charge_kind(duplicate_charge) == Some(RssKind::Anon)
        && accounting.rss_anon_pages() == 1;
    let _ = accounting.remove_charge(duplicate_charge);
    result.is_err() && mappings_removed && accounting_rolled_back
}

/// Copy-on-write mapping backend.
///
/// This corresponds to the `MAP_PRIVATE` flag.
pub struct CowBackend {
    start: VirtAddr,
    size: PageSize,
    file: Option<PrivateFileMapping>,
    name: Option<String>,
    shared: bool,
}

impl Clone for CowBackend {
    fn clone(&self) -> Self {
        Self {
            start: self.start,
            size: self.size,
            file: self.file.clone(),
            name: self.name.clone(),
            shared: self.shared,
        }
    }
}

impl CowBackend {
    pub fn is_anonymous(&self) -> bool {
        self.file.is_none()
    }

    pub fn with_start(&self, new_start: VirtAddr) -> Self {
        Self {
            start: new_start,
            size: self.size,
            file: self.file.clone(),
            name: self.name.clone(),
            shared: self.shared,
        }
    }

    fn rss_kind_for_fault(&self, access_flags: MappingFlags) -> RssKind {
        let is_file = self.file.is_some();
        let is_read = !access_flags.contains(MappingFlags::WRITE);
        if is_file && is_read {
            RssKind::File
        } else {
            RssKind::Anon
        }
    }

    /// PTE flags applied by [`super::Backend::protect`].
    ///
    /// File-backed private mappings keep PTEs read-only after `mprotect(+W)` so
    /// the first store still faults into [`Self::handle_cow_fault`] for RSS
    /// reclassify without touching charge at mprotect time (fork sibling case).
    pub(super) fn pte_flags_for_protect(&self, new_flags: MappingFlags) -> MappingFlags {
        if self.file.is_some() && new_flags.contains(MappingFlags::WRITE) {
            new_flags - MappingFlags::WRITE
        } else {
            new_flags
        }
    }

    /// PTE flags for fault-in of file-backed private pages.
    ///
    /// Read faults keep PTEs read-only so the first store still faults into
    /// [`Self::handle_cow_fault`] for RSS reclassify (Linux `PAGE_COPY` path).
    fn pte_flags_for_fault_in(
        &self,
        vma_flags: MappingFlags,
        access_flags: MappingFlags,
    ) -> MappingFlags {
        if self.file.is_some() && !access_flags.contains(MappingFlags::WRITE) {
            vma_flags - MappingFlags::WRITE
        } else {
            vma_flags
        }
    }

    /// True when VMA allows write but the resident PTE is still read-only (Cow
    /// deferred first-write path after `mprotect(+W)` on a file-backed mapping).
    fn cow_deferred_file_write(&self, vma_flags: MappingFlags, pte_flags: MappingFlags) -> bool {
        self.file.is_some()
            && vma_flags.contains(MappingFlags::WRITE)
            && !pte_flags.contains(MappingFlags::WRITE)
    }

    fn deinit_frame(&self, paddr: PhysAddr) {
        FRAME_TABLE.lock().remove_frame(paddr);
        dealloc_frame(paddr, self.size);
    }

    /// File→Anon RSS after a private mapping write fault.
    fn reclassify_or_adopt_cow_write(&self, acct: &MemoryAccounting, vaddr: VirtAddr) {
        let page_vaddr = vaddr.align_down(self.size);
        let pre_kind = acct.charge_kind(page_vaddr);
        if acct.cow_file_write_to_anon(page_vaddr) {
            return;
        }
        if page_vaddr != vaddr && acct.cow_file_write_to_anon(vaddr) {
            return;
        }
        let post_kind = acct.charge_kind(page_vaddr);
        warn!(
            "COW write at {vaddr:?} could not reclassify RSS (pre={pre_kind:?} post={post_kind:?})"
        );
    }

    fn alloc_new_frame(&self, initialization: PageInitialization) -> AxResult<PhysAddr> {
        let frame = alloc_frame(initialization, self.size)?;
        FRAME_TABLE.lock().init_frame(frame);
        Ok(frame)
    }

    fn alloc_new_at(
        &self,
        vaddr: VirtAddr,
        flags: MappingFlags,
        access_flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult<PhysAddr> {
        let kind = self.rss_kind_for_fault(access_flags);
        let frame = self.alloc_new_frame(PageInitialization::Zeroed)?;

        if let Some(file_mapping) = &self.file {
            let buf = unsafe {
                slice::from_raw_parts_mut(phys_to_virt(frame).as_mut_ptr(), self.size as _)
            };
            if let Err(err) = file_mapping.read_page(vaddr, buf) {
                self.deinit_frame(frame);
                return Err(err);
            }
        }
        let pte_flags = self.pte_flags_for_fault_in(flags, access_flags);
        if let Err(err) = pt.map(vaddr, frame, self.size, pte_flags) {
            self.deinit_frame(frame);
            return Err(err.into());
        }
        if let Some(acct) = acct
            && let Err(error) = acct.record_charge(vaddr, kind)
        {
            if pt.unmap(vaddr).is_err() {
                return Err(AxError::BadState);
            }
            self.deinit_frame(frame);
            return Err(error);
        }
        Ok(frame)
    }

    /// Fill a run of consecutive not-mapped FILE-backed pages with a single
    /// `read_at` (readahead), then allocate + map each page.
    fn alloc_file_run(
        &self,
        run: &[VirtAddr],
        flags: MappingFlags,
        access_flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult<usize> {
        let Some(file_mapping) = &self.file else {
            return self.alloc_pages_individually(run, flags, access_flags, acct, pt);
        };
        let ps = self.size as usize;
        let v0 = run[0];
        if v0.as_usize() < file_mapping.vaddr_base().as_usize() {
            return self.alloc_pages_individually(run, flags, access_flags, acct, pt);
        }
        let n = run.len();
        let total = n * ps;
        let mut buf = alloc::vec![0u8; total];
        file_mapping.read_run(v0, &mut buf)?;
        let kind = self.rss_kind_for_fault(access_flags);
        let mut mapped = alloc::vec::Vec::new();
        mapped
            .try_reserve_exact(run.len())
            .map_err(|_| AxError::NoMemory)?;
        for (k, &addr) in run.iter().enumerate() {
            let frame = match self.alloc_new_frame(PageInitialization::Uninitialized) {
                Ok(frame) => frame,
                Err(error) => {
                    return if self.rollback_fault_run(&mapped, acct, pt) {
                        Err(error)
                    } else {
                        Err(AxError::BadState)
                    };
                }
            };
            let dst = unsafe { slice::from_raw_parts_mut(phys_to_virt(frame).as_mut_ptr(), ps) };
            dst.copy_from_slice(&buf[k * ps..(k + 1) * ps]);
            let pte_flags = self.pte_flags_for_fault_in(flags, access_flags);
            if let Err(err) = pt.map(addr, frame, self.size, pte_flags) {
                self.deinit_frame(frame);
                return if self.rollback_fault_run(&mapped, acct, pt) {
                    Err(err.into())
                } else {
                    Err(AxError::BadState)
                };
            }
            if let Some(acct) = acct
                && let Err(error) = acct.record_charge(addr, kind)
            {
                let current_rolled_back = if pt.unmap(addr).is_ok() {
                    self.deinit_frame(frame);
                    true
                } else {
                    false
                };
                let previous_rolled_back = self.rollback_fault_run(&mapped, Some(acct), pt);
                return if current_rolled_back && previous_rolled_back {
                    Err(error)
                } else {
                    Err(AxError::BadState)
                };
            }
            mapped.push((addr, frame));
        }
        Ok(n)
    }

    fn alloc_pages_individually(
        &self,
        run: &[VirtAddr],
        flags: MappingFlags,
        access_flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult<usize> {
        let mut mapped = alloc::vec::Vec::new();
        mapped
            .try_reserve_exact(run.len())
            .map_err(|_| AxError::NoMemory)?;
        for &addr in run {
            match self.alloc_new_at(addr, flags, access_flags, acct, pt) {
                Ok(frame) => mapped.push((addr, frame)),
                Err(error) => {
                    return if self.rollback_fault_run(&mapped, acct, pt) {
                        Err(error)
                    } else {
                        Err(AxError::BadState)
                    };
                }
            }
        }
        Ok(run.len())
    }

    fn rollback_fault_run(
        &self,
        mapped: &[(VirtAddr, PhysAddr)],
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> bool {
        let mut restored = true;
        for &(vaddr, frame) in mapped.iter().rev() {
            if pt.unmap(vaddr).is_err() {
                restored = false;
                continue;
            }
            if acct.is_some_and(|acct| acct.remove_charge(vaddr).is_none()) {
                restored = false;
            }
            self.deinit_frame(frame);
        }
        restored
    }

    fn handle_cow_fault(
        &self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        vma_flags: MappingFlags,
        pte_flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        let frame_table = FRAME_TABLE.lock();
        let frame = frame_table
            .get_frame_ref(paddr)
            .ok_or(AxError::BadAddress)?;
        drop(frame_table);
        let mut frame = frame.lock();
        assert!(frame.count() > 0, "invalid frame reference count");
        match frame.count() {
            1 => {
                pt.protect(vaddr, vma_flags)?;
                let defer_write = self.cow_deferred_file_write(vma_flags, pte_flags);
                if defer_write && let Some(acct) = acct {
                    self.reclassify_or_adopt_cow_write(acct, vaddr);
                }
                return Ok(());
            }
            _ => {
                let new_frame = self.alloc_new_frame(PageInitialization::Uninitialized)?;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        phys_to_virt(paddr).as_ptr(),
                        phys_to_virt(new_frame).as_mut_ptr(),
                        self.size as _,
                    );
                }
                if let Err(err) = pt.remap(vaddr, new_frame, vma_flags) {
                    self.deinit_frame(new_frame);
                    return Err(err.into());
                }
                if self.file.is_some()
                    && let Some(acct) = acct
                {
                    self.reclassify_or_adopt_cow_write(acct, vaddr);
                }
                release_frame(&mut frame, paddr, self.size);
            }
        }

        Ok(())
    }

    /// Unmap one resident page and drop its per-VA RSS charge.
    ///
    /// Regular munmap / MAP_FIXED / shrink paths only; [`super::AddrSpace::move_pages`]
    /// migrates PTEs directly and uses [`MemoryAccounting::move_charge`] instead.
    fn unmap_page(
        &self,
        addr: VirtAddr,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        match pt.unmap(addr) {
            Ok((frame, _flags, page_size)) => {
                assert_eq!(page_size, self.size);
                if let Some(acct) = acct {
                    acct.remove_charge(addr);
                }
                let frame_ref = FRAME_TABLE
                    .lock()
                    .get_frame_ref(frame)
                    .ok_or(AxError::BadAddress)?;
                let mut frame_ref = frame_ref.lock();
                release_frame(&mut frame_ref, frame, self.size);
            }
            Err(PagingError::NotMapped) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub fn file_info(&self) -> AxResult<BackendFileInfo> {
        if let Some(mapping) = &self.file {
            let info = mapping.info(self.start)?;
            return Ok(BackendFileInfo {
                path: info.file.path,
                offset: Some(info.offset),
                inode: Some(info.file.inode),
                dev: Some(info.file.device),
                shared: self.shared,
            });
        }
        if let Some(name) = &self.name {
            return Ok(BackendFileInfo {
                path: name.clone(),
                offset: None,
                inode: None,
                dev: None,
                shared: self.shared,
            });
        }
        Err(AxError::InvalidInput)
    }
}

impl BackendOps for CowBackend {
    fn page_size(&self) -> PageSize {
        self.size
    }

    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        _acct: Option<&MemoryAccounting>,
        _pt: &mut PageTableCursor,
    ) -> AxResult {
        debug!("Cow::map: {range:?} {flags:?}",);
        Ok(())
    }

    fn unmap(
        &self,
        range: VirtAddrRange,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        debug!("Cow::unmap: {range:?}");
        for addr in pages_in(range, self.size)? {
            self.unmap_page(addr, acct, pt)?;
        }
        Ok(())
    }

    fn populate(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        access_flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult<(usize, Option<PopulateCallback>)> {
        let mut pages = 0;
        // Batch consecutive not-mapped FILE-backed pages into one readahead read.
        let addrs: alloc::vec::Vec<VirtAddr> = pages_in(range, self.size)?.collect();
        let mut i = 0;
        while i < addrs.len() {
            let addr = addrs[i];
            match pt.query(addr) {
                Ok((paddr, page_flags, page_size)) => {
                    assert_eq!(self.size, page_size);
                    if access_flags.contains(MappingFlags::WRITE)
                        && !page_flags.contains(MappingFlags::WRITE)
                    {
                        self.handle_cow_fault(addr, paddr, flags, page_flags, acct, pt)?;
                        pages += 1;
                    } else if page_flags.contains(access_flags) {
                        pages += 1;
                    }
                    i += 1;
                }
                Err(PagingError::NotMapped) => {
                    if self.file.is_some() {
                        let run_start = i;
                        while i < addrs.len()
                            && matches!(pt.query(addrs[i]), Err(PagingError::NotMapped))
                        {
                            i += 1;
                        }
                        pages += self.alloc_file_run(
                            &addrs[run_start..i],
                            flags,
                            access_flags,
                            acct,
                            pt,
                        )?;
                    } else {
                        self.alloc_new_at(addr, flags, access_flags, acct, pt)?;
                        pages += 1;
                        i += 1;
                    }
                }
                Err(_) => return Err(AxError::BadAddress),
            }
        }
        Ok((pages, None))
    }

    fn clone_map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        old_pt: &mut PageTableCursor,
        new_pt: &mut PageTableCursor,
        _new_aspace: &Arc<Mutex<AddrSpace>>,
        acct: CloneMapAccounting<'_>,
    ) -> AxResult<Backend> {
        struct ClonePagePlan {
            vaddr: VirtAddr,
            paddr: PhysAddr,
            old_flags: MappingFlags,
            frame: Arc<SpinNoIrq<FrameRefCnt>>,
            copy_charge: bool,
        }

        fn rollback(
            pages: &[ClonePagePlan],
            old_pt: &mut PageTableCursor,
            new_pt: &mut PageTableCursor,
            child: Option<&MemoryAccounting>,
            page_size: PageSize,
        ) -> AxResult<()> {
            let mut rollback_failed = false;
            for page in pages.iter().rev() {
                if page.copy_charge
                    && child
                        .and_then(|accounting| accounting.remove_charge(page.vaddr))
                        .is_none()
                {
                    rollback_failed = true;
                }
                if new_pt.unmap(page.vaddr).is_err() {
                    rollback_failed = true;
                }
                if old_pt.protect(page.vaddr, page.old_flags).is_err() {
                    rollback_failed = true;
                }
                release_frame(&mut page.frame.lock(), page.paddr, page_size);
            }
            if rollback_failed {
                Err(AxError::BadState)
            } else {
                Ok(())
            }
        }

        let cow_flags = flags - MappingFlags::WRITE;
        let mut plans = alloc::vec::Vec::new();
        plans
            .try_reserve_exact(range.size() / usize::from(self.size))
            .map_err(|_| AxError::NoMemory)?;

        for vaddr in pages_in(range, self.size)? {
            match old_pt.query(vaddr) {
                Ok((paddr, pte_flags, page_size)) => {
                    assert_eq!(page_size, self.size);
                    let frame = FRAME_TABLE
                        .lock()
                        .get_frame_ref(paddr)
                        .ok_or(AxError::BadAddress)?;
                    {
                        let frame_ref = frame.lock();
                        assert!(frame_ref.count() > 0, "referencing unreferenced frame");
                        frame_ref.count().checked_add(1).ok_or(AxError::NoMemory)?;
                    }
                    plans.push(ClonePagePlan {
                        vaddr,
                        paddr,
                        old_flags: pte_flags,
                        frame,
                        copy_charge: acct
                            .parent
                            .is_some_and(|parent| parent.charge_kind(vaddr).is_some()),
                    });
                }
                Err(PagingError::NotMapped) => {}
                Err(_) => return Err(AxError::BadAddress),
            };
        }

        let mut committed = alloc::vec::Vec::new();
        committed
            .try_reserve_exact(plans.len())
            .map_err(|_| AxError::NoMemory)?;
        for plan in plans {
            if plan.frame.lock().try_add().is_err() {
                rollback(&committed, old_pt, new_pt, acct.child, self.size)?;
                return Err(AxError::NoMemory);
            }
            if let Err(error) = old_pt.protect(plan.vaddr, cow_flags) {
                release_frame(&mut plan.frame.lock(), plan.paddr, self.size);
                rollback(&committed, old_pt, new_pt, acct.child, self.size)?;
                return Err(error.into());
            }
            if let Err(error) = new_pt.map(plan.vaddr, plan.paddr, self.size, cow_flags) {
                let parent_restored = old_pt.protect(plan.vaddr, plan.old_flags).is_ok();
                release_frame(&mut plan.frame.lock(), plan.paddr, self.size);
                rollback(&committed, old_pt, new_pt, acct.child, self.size)?;
                return if parent_restored {
                    Err(error.into())
                } else {
                    Err(AxError::BadState)
                };
            }
            if plan.copy_charge
                && let (Some(parent), Some(child)) = (acct.parent, acct.child)
                && let Err(error) = child.copy_charge_from(parent, plan.vaddr)
            {
                let child_unmapped = new_pt.unmap(plan.vaddr).is_ok();
                let parent_restored = old_pt.protect(plan.vaddr, plan.old_flags).is_ok();
                release_frame(&mut plan.frame.lock(), plan.paddr, self.size);
                rollback(&committed, old_pt, new_pt, acct.child, self.size)?;
                return if child_unmapped && parent_restored {
                    Err(error)
                } else {
                    Err(AxError::BadState)
                };
            }
            committed.push(plan);
        }
        Ok(Backend::Cow(self.clone()))
    }

    fn split(&mut self, align_diff: usize) -> Option<Backend> {
        assert!(align_diff.is_multiple_of(PAGE_SIZE_4K));
        if align_diff == 0 {
            return None;
        }
        let mut right = self.clone();
        right.start = self.start + align_diff;
        Some(Backend::Cow(right))
    }
}

impl Backend {
    pub fn new_cow(
        start: VirtAddr,
        size: PageSize,
        file: FileBackend,
        file_start: u64,
        file_end: Option<u64>,
        shared: bool,
    ) -> Self {
        Self::Cow(CowBackend {
            start: start.align_down_4k(),
            size,
            file: Some(PrivateFileMapping::new(
                Arc::new(KernelVmFile(file)),
                start,
                file_start,
                file_end,
            )),
            name: None,
            shared,
        })
    }

    pub fn new_alloc(start: VirtAddr, size: PageSize, name: &str) -> Self {
        Self::Cow(CowBackend {
            start: start.align_down_4k(),
            size,
            file: None,
            name: Some(name.to_string()),
            shared: false,
        })
    }
}
