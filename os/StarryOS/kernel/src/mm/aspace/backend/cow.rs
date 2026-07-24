use alloc::{
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
use hashbrown::HashMap;
use starry_mm::{
    CowFrameReferences, CowRelease, PageInitialization, PrivateFileMapping, VmFile, VmFileInfo,
};

use super::{
    AddrSpace, Backend, BackendFileInfo, BackendOps, CloneMapAccounting, MemoryAccounting,
    PopulateCallback, RssKind, alloc_frame, dealloc_frame, pages_in,
};

const FILE_READAHEAD_PAGES: usize = 16;

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

fn release_frame(paddr: PhysAddr, page_size: PageSize) {
    let release = FRAME_TABLE
        .lock()
        .release(paddr)
        .expect("releasing a registered copy-on-write frame");
    if release == CowRelease::LastReference {
        dealloc_frame(paddr, page_size);
    }
}

pub(super) fn retain_frame_for_transaction(paddr: PhysAddr) -> AxResult<()> {
    FRAME_TABLE.lock().try_retain(paddr)
}

pub(super) fn release_transaction_frame(paddr: PhysAddr, page_size: PageSize) {
    release_frame(paddr, page_size);
}

pub(super) fn frame_kind(paddr: PhysAddr) -> Option<RssKind> {
    FRAME_TABLE.lock().kind(paddr)
}

struct CowFrameState {
    references: CowFrameReferences,
    rss_kind: RssKind,
}

struct CowFrameTable {
    table: Option<HashMap<usize, CowFrameState>>,
}

impl CowFrameTable {
    const fn new() -> Self {
        Self { table: None }
    }

    fn kind(&self, paddr: PhysAddr) -> Option<RssKind> {
        self.table
            .as_ref()?
            .get(&paddr.as_usize())
            .map(|state| state.rss_kind)
    }

    fn count(&self, paddr: PhysAddr) -> Option<u32> {
        self.table
            .as_ref()?
            .get(&paddr.as_usize())
            .map(|state| state.references.count())
    }

    fn init_frame(&mut self, paddr: PhysAddr, rss_kind: RssKind) -> AxResult {
        let table = self.table.get_or_insert_with(HashMap::new);
        let key = paddr.as_usize();
        if table.contains_key(&key) {
            return Err(AxError::BadState);
        }
        table.try_reserve(1).map_err(|_| AxError::NoMemory)?;
        table.insert(
            key,
            CowFrameState {
                references: CowFrameReferences::new(),
                rss_kind,
            },
        );
        Ok(())
    }

    fn try_retain(&mut self, paddr: PhysAddr) -> AxResult {
        self.table
            .as_mut()
            .and_then(|table| table.get_mut(&paddr.as_usize()))
            .ok_or(AxError::BadAddress)?
            .references
            .try_add()
            .map_err(|_| AxError::NoMemory)
    }

    fn release(&mut self, paddr: PhysAddr) -> Option<CowRelease> {
        let table = self.table.as_mut()?;
        let key = paddr.as_usize();
        let release = table.get_mut(&key)?.references.release();
        if release == CowRelease::LastReference {
            table.remove(&key);
        }
        Some(release)
    }
}

static FRAME_TABLE: SpinNoIrq<CowFrameTable> = SpinNoIrq::new(CowFrameTable::new());

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
    accounting.set_resident_pages_for_test(RssKind::Anon, u64::MAX - 1);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let result = backend.alloc_pages_individually(
        &[vaddr, vaddr + PAGE_SIZE_4K],
        flags,
        MappingFlags::READ,
        Some(&accounting),
        &mut page_table.cursor(),
    );
    let mappings_removed = [vaddr, vaddr + PAGE_SIZE_4K].into_iter().all(|address| {
        matches!(
            page_table.cursor().query(address),
            Err(PagingError::NotMapped)
        )
    });
    result.is_err() && mappings_removed && accounting.rss_anon_pages() == u64::MAX - 1
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
        release_frame(paddr, self.size);
    }

    fn alloc_new_frame(
        &self,
        initialization: PageInitialization,
        rss_kind: RssKind,
    ) -> AxResult<PhysAddr> {
        let frame = alloc_frame(initialization, self.size)?;
        if let Err(error) = FRAME_TABLE.lock().init_frame(frame, rss_kind) {
            dealloc_frame(frame, self.size);
            return Err(error);
        }
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
        let frame = self.alloc_new_frame(PageInitialization::Zeroed, kind)?;

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
            && let Err(error) = acct.inc(kind, 1)
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
        let kind = self.rss_kind_for_fault(access_flags);
        let mut mapped = alloc::vec::Vec::new();
        mapped
            .try_reserve_exact(run.len())
            .map_err(|_| AxError::NoMemory)?;
        let mut buffer = alloc::vec::Vec::new();
        for chunk in run.chunks(FILE_READAHEAD_PAGES) {
            let bytes = chunk.len().checked_mul(ps).ok_or(AxError::NoMemory)?;
            buffer.clear();
            buffer
                .try_reserve_exact(bytes)
                .map_err(|_| AxError::NoMemory)?;
            buffer.resize(bytes, 0);
            file_mapping.read_run(chunk[0], &mut buffer)?;

            for (index, &addr) in chunk.iter().enumerate() {
                let frame = match self.alloc_new_frame(PageInitialization::Uninitialized, kind) {
                    Ok(frame) => frame,
                    Err(error) => {
                        return if self.rollback_fault_run(&mapped, acct, pt) {
                            Err(error)
                        } else {
                            Err(AxError::BadState)
                        };
                    }
                };
                let dst =
                    unsafe { slice::from_raw_parts_mut(phys_to_virt(frame).as_mut_ptr(), ps) };
                dst.copy_from_slice(&buffer[index * ps..(index + 1) * ps]);
                let pte_flags = self.pte_flags_for_fault_in(flags, access_flags);
                if let Err(error) = pt.map(addr, frame, self.size, pte_flags) {
                    self.deinit_frame(frame);
                    return if self.rollback_fault_run(&mapped, acct, pt) {
                        Err(error.into())
                    } else {
                        Err(AxError::BadState)
                    };
                }
                if let Some(acct) = acct
                    && let Err(error) = acct.inc(kind, 1)
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
        }
        Ok(run.len())
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
            let kind = FRAME_TABLE.lock().kind(frame);
            if let (Some(acct), Some(kind)) = (acct, kind)
                && acct.dec(kind, 1).is_err()
            {
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
        let (reference_count, old_kind) = {
            let table = FRAME_TABLE.lock();
            (
                table.count(paddr).ok_or(AxError::BadAddress)?,
                table.kind(paddr).ok_or(AxError::BadAddress)?,
            )
        };
        match reference_count {
            1 => {
                pt.protect(vaddr, vma_flags)?;
                let defer_write = self.cow_deferred_file_write(vma_flags, pte_flags);
                if defer_write && let Some(acct) = acct {
                    let mut table = FRAME_TABLE.lock();
                    let Some(state) = table
                        .table
                        .as_mut()
                        .and_then(|frames| frames.get_mut(&paddr.as_usize()))
                    else {
                        pt.protect(vaddr, pte_flags)
                            .map_err(|_| AxError::BadState)?;
                        return Err(AxError::BadState);
                    };
                    if let Err(error) = acct.reclassify(old_kind, RssKind::Anon, 1) {
                        drop(table);
                        pt.protect(vaddr, pte_flags)
                            .map_err(|_| AxError::BadState)?;
                        return Err(error);
                    }
                    state.rss_kind = RssKind::Anon;
                }
                return Ok(());
            }
            _ => {
                let new_kind = if self.file.is_some() {
                    RssKind::Anon
                } else {
                    old_kind
                };
                let new_frame =
                    self.alloc_new_frame(PageInitialization::Uninitialized, new_kind)?;
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
                if let Some(acct) = acct
                    && let Err(error) = acct.reclassify(old_kind, new_kind, 1)
                {
                    let mapping_restored = pt.remap(vaddr, paddr, pte_flags).is_ok();
                    if mapping_restored {
                        self.deinit_frame(new_frame);
                        return Err(error);
                    }
                    return Err(AxError::BadState);
                }
                release_frame(paddr, self.size);
            }
        }

        Ok(())
    }

    /// Unmaps one resident page and releases its frame-owned RSS category.
    fn unmap_page(
        &self,
        addr: VirtAddr,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        match pt.unmap(addr) {
            Ok((frame, flags, page_size)) => {
                assert_eq!(page_size, self.size);
                let Some(kind) = FRAME_TABLE.lock().kind(frame) else {
                    pt.map(addr, frame, page_size, flags)
                        .map_err(|_| AxError::BadState)?;
                    return Err(AxError::BadState);
                };
                if let Some(acct) = acct
                    && let Err(error) = acct.dec(kind, 1)
                {
                    pt.map(addr, frame, page_size, flags)
                        .map_err(|_| AxError::BadState)?;
                    return Err(error);
                }
                release_frame(frame, self.size);
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
        let mut file_run = alloc::vec::Vec::new();
        file_run
            .try_reserve_exact(FILE_READAHEAD_PAGES)
            .map_err(|_| AxError::NoMemory)?;
        for addr in pages_in(range, self.size)? {
            match pt.query(addr) {
                Ok((paddr, page_flags, page_size)) => {
                    if !file_run.is_empty() {
                        pages += self.alloc_file_run(&file_run, flags, access_flags, acct, pt)?;
                        file_run.clear();
                    }
                    assert_eq!(self.size, page_size);
                    if access_flags.contains(MappingFlags::WRITE)
                        && !page_flags.contains(MappingFlags::WRITE)
                    {
                        self.handle_cow_fault(addr, paddr, flags, page_flags, acct, pt)?;
                        pages += 1;
                    } else if page_flags.contains(access_flags) {
                        pages += 1;
                    }
                }
                Err(PagingError::NotMapped) => {
                    if self.file.is_some() {
                        file_run.push(addr);
                        if file_run.len() == FILE_READAHEAD_PAGES {
                            pages +=
                                self.alloc_file_run(&file_run, flags, access_flags, acct, pt)?;
                            file_run.clear();
                        }
                    } else {
                        self.alloc_new_at(addr, flags, access_flags, acct, pt)?;
                        pages += 1;
                    }
                }
                Err(_) => return Err(AxError::BadAddress),
            }
        }
        if !file_run.is_empty() {
            pages += self.alloc_file_run(&file_run, flags, access_flags, acct, pt)?;
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
            rss_kind: RssKind,
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
                if new_pt.unmap(page.vaddr).is_ok() {
                    if child.is_some_and(|accounting| accounting.dec(page.rss_kind, 1).is_err()) {
                        rollback_failed = true;
                    }
                    release_frame(page.paddr, page_size);
                } else {
                    rollback_failed = true;
                }
                if old_pt.protect(page.vaddr, page.old_flags).is_err() {
                    rollback_failed = true;
                }
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
                    let table = FRAME_TABLE.lock();
                    let count = table.count(paddr).ok_or(AxError::BadAddress)?;
                    count.checked_add(1).ok_or(AxError::NoMemory)?;
                    let rss_kind = table.kind(paddr).ok_or(AxError::BadAddress)?;
                    plans.push(ClonePagePlan {
                        vaddr,
                        paddr,
                        old_flags: pte_flags,
                        rss_kind,
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
            if FRAME_TABLE.lock().try_retain(plan.paddr).is_err() {
                rollback(&committed, old_pt, new_pt, acct.child, self.size)?;
                return Err(AxError::NoMemory);
            }
            if let Err(error) = old_pt.protect(plan.vaddr, cow_flags) {
                release_frame(plan.paddr, self.size);
                rollback(&committed, old_pt, new_pt, acct.child, self.size)?;
                return Err(error.into());
            }
            if let Err(error) = new_pt.map(plan.vaddr, plan.paddr, self.size, cow_flags) {
                let parent_restored = old_pt.protect(plan.vaddr, plan.old_flags).is_ok();
                release_frame(plan.paddr, self.size);
                rollback(&committed, old_pt, new_pt, acct.child, self.size)?;
                return if parent_restored {
                    Err(error.into())
                } else {
                    Err(AxError::BadState)
                };
            }
            if let Some(child) = acct.child
                && let Err(error) = child.inc(plan.rss_kind, 1)
            {
                let child_unmapped = new_pt.unmap(plan.vaddr).is_ok();
                let parent_restored = old_pt.protect(plan.vaddr, plan.old_flags).is_ok();
                if child_unmapped {
                    release_frame(plan.paddr, self.size);
                }
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
