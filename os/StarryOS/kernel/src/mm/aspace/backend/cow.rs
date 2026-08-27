use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{cell::Cell, slice};

use ax_fs_ng::vfs::FileBackend;
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange, align_down_4k};
use ax_runtime::hal::{
    mem::phys_to_virt,
    paging::{MappingFlags, PageTable, PagingError},
};

use super::{
    AddrSpace, Backend, BackendFileInfo, BackendOps, CloneMapAccounting, MemoryAccounting,
    PopulateCallback, RssKind, alloc_frame, dealloc_frame, pages_in,
};
use crate::{
    StarryError, StarryResult,
    sync::{IrqMutex, Mutex},
};

struct FrameRefCnt {
    /// Number of address spaces sharing this frame COW. A `u8` overflowed at 255
    /// sharers — a read-only libc/text frame shared by ~250 forked processes
    /// (e.g. `hackbench -P g10`) tripped it and `fork()` failed with EFAULT. Linux
    /// uses a 32-bit refcount; `u32` (4 billion sharers) is effectively unbounded
    /// here, and the overflow path now returns `NoMemory` (ENOMEM), not EFAULT.
    count: u32,
}

impl FrameRefCnt {
    fn drop_frame(&mut self, paddr: PhysAddr, page_size: usize) {
        assert!(self.count > 0, "dropping unreferenced frame");
        self.count -= 1;
        if self.count == 0 {
            FRAME_TABLE.lock().remove_frame(paddr);
            dealloc_frame(paddr, page_size);
        }
    }
}

struct FrameTableRefCount {
    table: BTreeMap<PhysAddr, Arc<IrqMutex<FrameRefCnt>>>,
}

impl FrameTableRefCount {
    const INITIAL_CNT: u32 = 1;

    const fn new() -> Self {
        Self {
            table: BTreeMap::new(),
        }
    }

    fn get_frame_ref(&mut self, paddr: PhysAddr) -> Option<Arc<IrqMutex<FrameRefCnt>>> {
        self.table.get(&paddr).cloned()
    }

    fn init_frame(&mut self, paddr: PhysAddr) {
        assert!(
            !self.table.contains_key(&paddr),
            "initializing already referenced frame"
        );
        self.table.insert(
            paddr,
            Arc::new(IrqMutex::new(FrameRefCnt {
                count: Self::INITIAL_CNT,
            })),
        );
    }

    fn remove_frame(&mut self, paddr: PhysAddr) {
        assert!(
            self.table.contains_key(&paddr),
            "removing unreferenced frame"
        );
        self.table.remove(&paddr);
    }
}

static FRAME_TABLE: IrqMutex<FrameTableRefCount> = IrqMutex::new(FrameTableRefCount::new());

fn cow_file_max_read_len(
    file_len: u64,
    file_end: Option<u64>,
    file_read_offset: u64,
    available: usize,
) -> StarryResult<usize> {
    let effective_end = match file_end {
        Some(end) => end,
        None => {
            if file_read_offset >= file_len {
                return Err(StarryError::BadAddress);
            }
            file_len
        }
    };
    Ok(effective_end
        .saturating_sub(file_read_offset)
        .min(available as u64) as usize)
}

fn cow_file_max_read(
    file: &FileBackend,
    file_end: Option<u64>,
    file_read_offset: u64,
    available: usize,
) -> StarryResult<usize> {
    let file_len = if file_end.is_none() { file.len()? } else { 0 };
    cow_file_max_read_len(file_len, file_end, file_read_offset, available)
}

#[cfg(all(test, not(axtest)))]
fn private_mmap_eof_check_for_test() -> bool {
    matches!(
        cow_file_max_read_len(4096, None, 4096, 4096),
        Err(StarryError::BadAddress)
    ) && matches!(cow_file_max_read_len(4096, None, 2048, 4096), Ok(2048))
        && matches!(
            cow_file_max_read_len(4096, Some(8192), 4096, 4096),
            Ok(4096)
        )
}

/// Copy-on-write mapping backend.
///
/// This corresponds to the `MAP_PRIVATE` flag.
pub struct CowBackend {
    start: VirtAddr,
    size: usize,
    file: Option<(FileBackend, VirtAddr, u64, Option<u64>)>,
    name: Option<String>,
    shared: bool,
    /// True after this address space upgrades the mapping to writable via
    /// `mprotect(+W)` or a writable `mmap` (per-aspace; fork inherits via
    /// [`Clone`]).
    write_upgraded: Cell<bool>,
}

impl Clone for CowBackend {
    fn clone(&self) -> Self {
        Self {
            start: self.start,
            size: self.size,
            file: self.file.clone(),
            name: self.name.clone(),
            shared: self.shared,
            write_upgraded: Cell::new(self.write_upgraded.get()),
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
            write_upgraded: Cell::new(self.write_upgraded.get()),
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
    /// Every private (Cow) mapping — file-backed AND anonymous — keeps its PTEs
    /// read-only after `mprotect(+W)`, so the first store faults into
    /// [`Self::handle_cow_fault`], which COW-breaks a shared frame (refcount > 1,
    /// after fork: copy + remap + drop the shared ref) or simply re-enables write
    /// on an exclusive frame (refcount == 1). Without this an anonymous COW-shared
    /// page got a writable PTE on the shared frame with no break, so a store in one
    /// forked process was visible in the other (inter-process corruption). File-backed
    /// mappings additionally use the deferred fault for RSS reclassify.
    pub(super) fn pte_flags_for_protect(&self, new_flags: MappingFlags) -> MappingFlags {
        if new_flags.contains(MappingFlags::WRITE) {
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

    fn alloc_new_frame(&self, zeroed: bool) -> StarryResult<PhysAddr> {
        let frame = alloc_frame(zeroed, self.size)?;
        FRAME_TABLE.lock().init_frame(frame);
        Ok(frame)
    }

    fn alloc_new_at(
        &self,
        vaddr: VirtAddr,
        flags: MappingFlags,
        access_flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTable,
    ) -> StarryResult {
        let kind = self.rss_kind_for_fault(access_flags);
        let frame = self.alloc_new_frame(true)?;

        if let Some((file, file_vaddr_base, file_start, file_end)) = &self.file {
            let buf = unsafe {
                slice::from_raw_parts_mut(phys_to_virt(frame).as_mut_ptr(), self.size as _)
            };
            // vaddr can be smaller than file_vaddr_base (at most 1 page) due to
            // non-aligned mappings; compute page-internal write offset accordingly.
            // The mapping invariant is: a virtual address `V` corresponds to
            // file offset `file_start + (V - file_vaddr_base)`. The file-backed
            // bytes of this page begin at buf[start] (= virtual address
            // `file_vaddr_base` when the page starts below it, i.e. the
            // unaligned first page), which therefore reads from `file_start`.
            // `saturating_sub` yields exactly that: 0 when vaddr < file_vaddr_base
            // (read from file_start) and the positive delta otherwise. Do NOT
            // subtract the gap here — doing so reads the segment's bytes from
            // the wrong offset and corrupts e.g. the dynamic linker's
            // .dynamic/GOT, making ld-musl jump to a null pointer.
            let start = file_vaddr_base.as_usize().saturating_sub(vaddr.as_usize());
            assert!(start < self.size as _);

            let file_read_offset =
                *file_start + vaddr.as_usize().saturating_sub(file_vaddr_base.as_usize()) as u64;
            let max_read =
                match cow_file_max_read(file, *file_end, file_read_offset, buf.len() - start) {
                    Ok(max_read) => max_read,
                    Err(err) => {
                        self.deinit_frame(frame);
                        return Err(err);
                    }
                };

            if let Err(err) = file.read_at(&mut &mut buf[start..start + max_read], file_read_offset)
            {
                self.deinit_frame(frame);
                return Err(err.into());
            }
        }
        let pte_flags = self.pte_flags_for_fault_in(flags, access_flags);
        if let Err(err) = pt.map_page(vaddr, frame, self.size, pte_flags) {
            self.deinit_frame(frame);
            return Err(err.into());
        }
        if let Some(acct) = acct {
            acct.record_charge(vaddr, kind)?;
        }
        Ok(())
    }

    /// Fill a run of consecutive not-mapped FILE-backed pages with a single
    /// `read_at` (readahead), then allocate + map each page.
    fn alloc_file_run(
        &self,
        run: &[VirtAddr],
        flags: MappingFlags,
        access_flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTable,
    ) -> StarryResult<usize> {
        let Some((file, file_vaddr_base, file_start, file_end)) = &self.file else {
            for &addr in run {
                self.alloc_new_at(addr, flags, access_flags, acct, pt)?;
            }
            return Ok(run.len());
        };
        let ps = self.size;
        let v0 = run[0];
        if v0.as_usize() < file_vaddr_base.as_usize() {
            for &addr in run {
                self.alloc_new_at(addr, flags, access_flags, acct, pt)?;
            }
            return Ok(run.len());
        }
        let n = run.len();
        let total = n * ps;
        let file_read_offset = file_start + (v0.as_usize() - file_vaddr_base.as_usize()) as u64;
        let max_read = cow_file_max_read(file, *file_end, file_read_offset, total)?;
        let mut buf = alloc::vec![0u8; total];
        if max_read > 0 {
            file.read_at(&mut &mut buf[..max_read], file_read_offset)?;
        }
        let kind = self.rss_kind_for_fault(access_flags);
        for (k, &addr) in run.iter().enumerate() {
            let frame = self.alloc_new_frame(false)?;
            let dst = unsafe { slice::from_raw_parts_mut(phys_to_virt(frame).as_mut_ptr(), ps) };
            dst.copy_from_slice(&buf[k * ps..(k + 1) * ps]);
            let pte_flags = self.pte_flags_for_fault_in(flags, access_flags);
            if let Err(err) = pt.map_page(addr, frame, self.size, pte_flags) {
                self.deinit_frame(frame);
                return Err(err.into());
            }
            if let Some(acct) = acct {
                acct.record_charge(addr, kind)?;
            }
        }
        Ok(n)
    }

    fn handle_cow_fault(
        &self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        vma_flags: MappingFlags,
        pte_flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTable,
    ) -> StarryResult {
        let mut frame_table = FRAME_TABLE.lock();
        let frame = frame_table
            .get_frame_ref(paddr)
            .ok_or(StarryError::BadAddress)?;
        drop(frame_table);
        let mut frame = frame.lock();
        assert!(frame.count > 0, "invalid frame reference count");
        debug_assert!(
            frame.count < u32::MAX,
            "frame reference count near overflow"
        );
        match frame.count {
            1 => {
                pt.protect_page(vaddr, vma_flags)?;
                let defer_write =
                    self.cow_deferred_file_write(vma_flags, pte_flags) && self.write_upgraded.get();
                if defer_write && let Some(acct) = acct {
                    self.reclassify_or_adopt_cow_write(acct, vaddr);
                }
                return Ok(());
            }
            _ => {
                let new_frame = self.alloc_new_frame(false)?;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        phys_to_virt(paddr).as_ptr(),
                        phys_to_virt(new_frame).as_mut_ptr(),
                        self.size as _,
                    );
                }
                if let Err(err) = pt.remap_page(vaddr, new_frame, vma_flags) {
                    self.deinit_frame(new_frame);
                    return Err(err.into());
                }
                if self.file.is_some()
                    && let Some(acct) = acct
                {
                    self.reclassify_or_adopt_cow_write(acct, vaddr);
                }
                frame.drop_frame(paddr, self.size);
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
        pt: &mut PageTable,
    ) -> StarryResult {
        if let Ok((frame, _flags, page_size)) = pt.unmap_page(addr) {
            assert_eq!(page_size, self.size);
            if let Some(acct) = acct {
                acct.remove_charge(addr);
            }
            let frame_ref = FRAME_TABLE
                .lock()
                .get_frame_ref(frame)
                .ok_or(StarryError::BadAddress)?;
            let mut frame_ref = frame_ref.lock();
            frame_ref.drop_frame(frame, self.size);
        }
        Ok(())
    }

    pub fn file_info(&self) -> StarryResult<BackendFileInfo> {
        let loc = self
            .file
            .as_ref()
            .map(|(file, file_vaddr_base, file_start, ..)| {
                (file.location(), *file_vaddr_base, *file_start)
            });
        if let Some((loc, file_vaddr_base, file_start)) = loc {
            let path = loc.absolute_path().map(|pb| pb.to_string())?;
            let inode = loc.inode();
            let dev = loc.metadata()?.device;
            // Same invariant as `alloc_new_at`: a virtual address maps to
            // `file_start + (vaddr - file_vaddr_base)`, clamped to file_start
            // for the unaligned first page (where self.start < file_vaddr_base).
            let offset = file_start
                + self
                    .start
                    .as_usize()
                    .saturating_sub(file_vaddr_base.as_usize()) as u64;
            let offset = align_down_4k(offset as usize) as u64;
            return Ok(BackendFileInfo {
                path,
                offset: Some(offset),
                inode: Some(inode),
                dev: Some(dev),
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
        Err(StarryError::InvalidInput)
    }
}

struct CowCloneTransaction<'a> {
    parent_page_table: &'a mut PageTable,
    parent_original_flags: Vec<(VirtAddr, MappingFlags)>,
    rollback: PageTableCowCloneRollback<'a>,
    start: VirtAddr,
    cloned_end: VirtAddr,
    page_size: usize,
    committed: bool,
}

impl<'a> CowCloneTransaction<'a> {
    fn new(
        parent_page_table: &'a mut PageTable,
        child_page_table: &'a mut PageTable,
        child_acct: Option<&'a MemoryAccounting>,
        start: VirtAddr,
        page_size: usize,
    ) -> Self {
        Self {
            parent_page_table,
            parent_original_flags: Vec::new(),
            rollback: PageTableCowCloneRollback {
                page_table: child_page_table,
                child_acct,
                page_size,
            },
            start,
            cloned_end: start,
            page_size,
            committed: false,
        }
    }

    fn page_table_mut(&mut self) -> &mut PageTable {
        self.rollback.page_table
    }

    fn protect_parent_page(
        &mut self,
        vaddr: VirtAddr,
        original_flags: MappingFlags,
        cow_flags: MappingFlags,
    ) -> StarryResult {
        if original_flags == cow_flags {
            return Ok(());
        }
        // Reserve rollback state before mutating the published parent PTE, so
        // bookkeeping OOM cannot make a failed clone visible to the parent.
        self.parent_original_flags
            .try_reserve(1)
            .map_err(|_| StarryError::NoMemory)?;
        self.parent_original_flags.push((vaddr, original_flags));
        self.parent_page_table
            .protect_page(vaddr, cow_flags)
            .map(|_| ())
            .map_err(Into::into)
    }

    fn record_cloned_page(&mut self, vaddr: VirtAddr) {
        self.cloned_end = vaddr + self.page_size;
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for CowCloneTransaction<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        while let Some((vaddr, original_flags)) = self.parent_original_flags.pop() {
            if let Err(err) = self.parent_page_table.protect_page(vaddr, original_flags) {
                warn!("failed to restore parent COW page {vaddr:?} during rollback: {err}");
            }
        }
        while self.cloned_end > self.start {
            self.cloned_end -= self.page_size;
            self.rollback.rollback_page(self.cloned_end);
        }
    }
}

struct PageTableCowCloneRollback<'a> {
    page_table: &'a mut PageTable,
    child_acct: Option<&'a MemoryAccounting>,
    page_size: usize,
}

impl PageTableCowCloneRollback<'_> {
    fn rollback_page(&mut self, vaddr: VirtAddr) {
        if let Some(child) = self.child_acct
            && child.charge_kind(vaddr).is_some()
        {
            child.remove_charge(vaddr);
        }

        let (paddr, _, page_size) = match self.page_table.query(vaddr) {
            Ok(mapping) => mapping,
            Err(PagingError::NotMapped) => return,
            Err(err) => {
                warn!("failed to query cloned COW page {vaddr:?} during rollback: {err}");
                return;
            }
        };
        assert_eq!(page_size, self.page_size);
        if let Err(err) = self.page_table.unmap_page(vaddr) {
            warn!("failed to unmap cloned COW page {vaddr:?} during rollback: {err}");
        }

        // The unpublished child page table is dropped after clone failure, so its
        // reference must be released even if the explicit PTE removal failed.
        let Some(frame) = FRAME_TABLE.lock().get_frame_ref(paddr) else {
            warn!("missing frame reference for cloned COW page {vaddr:?} during rollback");
            return;
        };
        let mut frame = frame.lock();
        assert!(frame.count > 1, "rolling back unshared COW frame");
        frame.drop_frame(paddr, self.page_size);
    }
}

impl BackendOps for CowBackend {
    fn page_size(&self) -> usize {
        self.size
    }

    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        _acct: Option<&MemoryAccounting>,
        _pt: &mut PageTable,
    ) -> StarryResult {
        debug!("Cow::map: {range:?} {flags:?}",);
        if self.file.is_some() && flags.contains(MappingFlags::WRITE) {
            self.write_upgraded.set(true);
        }
        Ok(())
    }

    fn on_protect(
        &self,
        _range: VirtAddrRange,
        new_flags: MappingFlags,
        _pt: &mut PageTable,
    ) -> StarryResult {
        if self.file.is_some() && new_flags.contains(MappingFlags::WRITE) {
            self.write_upgraded.set(true);
        }
        Ok(())
    }

    fn unmap(
        &self,
        range: VirtAddrRange,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTable,
    ) -> StarryResult {
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
        pt: &mut PageTable,
    ) -> StarryResult<(usize, Option<PopulateCallback>)> {
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
                Err(_) => return Err(StarryError::BadAddress),
            }
        }
        Ok((pages, None))
    }

    fn clone_map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        old_pt: &mut PageTable,
        new_pt: &mut PageTable,
        _new_aspace: &Arc<Mutex<AddrSpace>>,
        acct: CloneMapAccounting<'_>,
    ) -> StarryResult<Backend> {
        let cow_flags = flags - MappingFlags::WRITE;
        let parent_acct = acct.parent;
        let child_acct = acct.child;
        let mut transaction =
            CowCloneTransaction::new(old_pt, new_pt, child_acct, range.start, self.size);

        for vaddr in pages_in(range, self.size)? {
            match transaction.parent_page_table.query(vaddr) {
                Ok((paddr, pte_flags, page_size)) => {
                    assert_eq!(page_size, self.size);
                    let frame = FRAME_TABLE
                        .lock()
                        .get_frame_ref(paddr)
                        .ok_or(StarryError::BadAddress)?;
                    let mut frame = frame.lock();
                    assert!(frame.count > 0, "referencing unreferenced frame");
                    // Overflow is effectively unreachable with a u32 refcount, but
                    // if it ever happens report it as ENOMEM (out of a shareable
                    // resource) rather than EFAULT — a fork hitting a real limit
                    // must not look like a bad pointer to userspace.
                    let new_count = match frame.count.checked_add(1) {
                        Some(c) => c,
                        None => {
                            warn!("frame reference count overflow");
                            drop(frame);
                            return Err(StarryError::NoMemory);
                        }
                    };
                    if let Err(err) = transaction.protect_parent_page(vaddr, pte_flags, cow_flags) {
                        drop(frame);
                        return Err(err);
                    }
                    if let Err(err) = transaction
                        .page_table_mut()
                        .map_page(vaddr, paddr, self.size, cow_flags)
                    {
                        drop(frame);
                        return Err(err.into());
                    }
                    // The child page table is not published yet and this frame is
                    // locked, so publish the extra reference only after its PTE exists.
                    frame.count = new_count;
                    transaction.record_cloned_page(vaddr);
                    drop(frame);
                    if let (Some(parent), Some(child)) = (parent_acct, child_acct)
                        && let Some(_kind) = parent.charge_kind(vaddr)
                    {
                        child.copy_charge_from(parent, vaddr)?;
                    }
                }
                Err(PagingError::NotMapped) => {}
                Err(_) => return Err(StarryError::BadAddress),
            };
        }
        transaction.commit();
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

    fn shrink_left(&mut self, shrink_size: usize) {
        assert!(shrink_size.is_multiple_of(PAGE_SIZE_4K));
        self.start += shrink_size;
    }

    fn shrink_right(&mut self, _shrink_size: usize) {}
}

impl Backend {
    pub fn new_cow(
        start: VirtAddr,
        size: usize,
        file: FileBackend,
        file_start: u64,
        file_end: Option<u64>,
        shared: bool,
    ) -> Self {
        Self::Cow(CowBackend {
            start: start.align_down_4k(),
            size,
            file: Some((file, start, file_start, file_end)),
            name: None,
            shared,
            write_upgraded: Cell::new(false),
        })
    }

    pub fn new_alloc(start: VirtAddr, size: usize, name: &str) -> Self {
        Self::Cow(CowBackend {
            start: start.align_down_4k(),
            size,
            file: None,
            name: Some(name.to_string()),
            shared: false,
            write_upgraded: Cell::new(false),
        })
    }
}

#[cfg(all(test, not(axtest)))]
fn cow_file_max_read_len_boundary_rules_hold_for_test() -> bool {
    // Zero-length file without an explicit end rejects any offset (offset 0 is
    // already >= file_len 0).
    matches!(cow_file_max_read_len(0, None, 0, 4096), Err(StarryError::BadAddress))
        // Offset past the file end without an explicit end is BadAddress.
        && matches!(
            cow_file_max_read_len(4096, None, 8192, 4096),
            Err(StarryError::BadAddress)
        )
        // Offset at exactly file_len without an explicit end is also BadAddress.
        && matches!(
            cow_file_max_read_len(4096, None, 4096, 4096),
            Err(StarryError::BadAddress)
        )
        // Explicit end below the file length caps the returned size.
        && matches!(cow_file_max_read_len(8192, Some(4096), 0, 8192), Ok(4096))
        // Returned size is always clamped by the caller-supplied capacity.
        && matches!(cow_file_max_read_len(8192, None, 0, 2048), Ok(2048))
        // Saturating subtraction never underflows when offset >= explicit end.
        && matches!(cow_file_max_read_len(8192, Some(4096), 8192, 4096), Ok(0))
        // Explicit end == offset yields zero (EOF reached within bounds).
        && matches!(cow_file_max_read_len(8192, Some(4096), 4096, 4096), Ok(0))
}

#[cfg(all(test, axtest))]
fn cow_clone_map_failure_restores_resources() -> bool {
    let start = VirtAddr::from(0x4000_0000);
    let second_page = start + PAGE_SIZE_4K;
    let mapping_size = 2 * PAGE_SIZE_4K;
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let backend = CowBackend {
        start,
        size: PAGE_SIZE_4K,
        file: None,
        name: Some("[cow-clone-rollback-test]".to_string()),
        shared: false,
        write_upgraded: Cell::new(false),
    };

    let Ok(mut parent) = AddrSpace::new_empty(start, mapping_size) else {
        return false;
    };
    if parent
        .map(
            start,
            mapping_size,
            flags,
            true,
            Backend::Cow(backend.clone()),
        )
        .is_err()
    {
        return false;
    }
    let Ok((first_frame, first_parent_flags, ..)) = parent.pt.query(start) else {
        return false;
    };
    let Ok((second_frame, second_parent_flags, ..)) = parent.pt.query(second_page) else {
        return false;
    };

    let Ok(child) = AddrSpace::new_empty(start, mapping_size) else {
        return false;
    };
    let child_aspace = Arc::new(Mutex::new(child));
    let mut child = child_aspace.lock();
    if child
        .pt
        .map_page(second_page, second_frame, PAGE_SIZE_4K, flags)
        .is_err()
    {
        return false;
    }
    let AddrSpace {
        pt: parent_pt,
        rss: parent_rss,
        ..
    } = &mut parent;
    let AddrSpace {
        pt: child_pt,
        rss: child_rss,
        ..
    } = &mut *child;

    let result = backend.clone_map(
        VirtAddrRange::from_start_size(start, mapping_size),
        flags,
        parent_pt,
        child_pt,
        &child_aspace,
        CloneMapAccounting {
            parent: Some(parent_rss),
            child: Some(child_rss),
        },
    );

    fn frame_ref_count(paddr: PhysAddr) -> Option<u32> {
        let frame = FRAME_TABLE.lock().get_frame_ref(paddr)?;
        Some(frame.lock().count)
    }

    let first_frame_count = frame_ref_count(first_frame);
    let second_frame_count = frame_ref_count(second_frame);

    matches!(
        result,
        Err(StarryError::Paging(PagingError::MappingConflict {
            vaddr,
            existing_paddr,
        })) if vaddr == second_page && existing_paddr == second_frame
    ) && matches!(child_pt.query(start), Err(PagingError::NotMapped))
        && matches!(
            parent_pt.query(start),
            Ok((paddr, parent_flags, page_size))
                if paddr == first_frame
                    && parent_flags == first_parent_flags
                    && page_size == PAGE_SIZE_4K
        )
        && matches!(
            parent_pt.query(second_page),
            Ok((paddr, parent_flags, page_size))
                if paddr == second_frame
                    && parent_flags == second_parent_flags
                    && page_size == PAGE_SIZE_4K
        )
        && matches!(
            child_pt.query(second_page),
            Ok((paddr, _, page_size))
                if paddr == second_frame && page_size == PAGE_SIZE_4K
        )
        && child_rss.charge_kind(start).is_none()
        && child_rss.charge_kind(second_page).is_none()
        && child_rss.rss_total_pages() == 0
        && first_frame_count == Some(FrameTableRefCount::INITIAL_CNT)
        && second_frame_count == Some(FrameTableRefCount::INITIAL_CNT)
}

#[cfg(all(test, axtest))]
fn cow_clone_failure_rollback_rules_hold_for_test() -> bool {
    cow_clone_map_failure_restores_resources()
}

#[cfg(test)]
mod tests {
    #[cfg(all(test, not(axtest)))]
    #[test]
    fn private_mmap_rejects_fault_at_file_eof() {
        assert!(super::private_mmap_eof_check_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn cow_file_max_read_len_boundary_rules_hold() {
        assert!(super::cow_file_max_read_len_boundary_rules_hold_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn cow_clone_failure_rollback_rules_hold() {
        assert!(super::cow_clone_failure_rollback_rules_hold_for_test());
    }
}
