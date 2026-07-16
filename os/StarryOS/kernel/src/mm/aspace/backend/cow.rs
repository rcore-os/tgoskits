use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
};
use core::{cell::Cell, slice};

use ax_errno::{AxError, AxResult};
use ax_fs_ng::vfs::FileBackend;
use ax_kspin::SpinNoIrq;
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange, align_down_4k};
use ax_runtime::hal::{
    mem::phys_to_virt,
    paging::{MappingFlags, PageSize, PageTableCursor, PagingError},
};
use ax_sync::PiMutex;

use super::{
    AddrSpace, Backend, BackendFileInfo, BackendOps, CloneMapAccounting, MemoryAccounting,
    PopulateCallback, RssKind, alloc_frame, dealloc_frame, pages_in,
};

struct FrameRefCnt {
    count: u8,
}

impl FrameRefCnt {
    fn drop_frame(&mut self, paddr: PhysAddr, page_size: PageSize) {
        assert!(self.count > 0, "dropping unreferenced frame");
        self.count -= 1;
        if self.count == 0 {
            FRAME_TABLE.lock().remove_frame(paddr);
            dealloc_frame(paddr, page_size);
        }
    }
}

struct FrameTableRefCount {
    table: BTreeMap<PhysAddr, Arc<SpinNoIrq<FrameRefCnt>>>,
}

impl FrameTableRefCount {
    const INITIAL_CNT: u8 = 1;

    const fn new() -> Self {
        Self {
            table: BTreeMap::new(),
        }
    }

    fn get_frame_ref(&mut self, paddr: PhysAddr) -> Option<Arc<SpinNoIrq<FrameRefCnt>>> {
        self.table.get(&paddr).cloned()
    }

    fn init_frame(&mut self, paddr: PhysAddr) {
        assert!(
            !self.table.contains_key(&paddr),
            "initializing already referenced frame"
        );
        self.table.insert(
            paddr,
            Arc::new(SpinNoIrq::new(FrameRefCnt {
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

static FRAME_TABLE: SpinNoIrq<FrameTableRefCount> = SpinNoIrq::new(FrameTableRefCount::new());

fn cow_file_max_read_len(
    file_len: u64,
    file_end: Option<u64>,
    file_read_offset: u64,
    available: usize,
) -> AxResult<usize> {
    let effective_end = match file_end {
        Some(end) => end,
        None => {
            if file_read_offset >= file_len {
                return Err(AxError::BadAddress);
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
) -> AxResult<usize> {
    let file_len = if file_end.is_none() { file.len()? } else { 0 };
    cow_file_max_read_len(file_len, file_end, file_read_offset, available)
}

#[cfg(axtest)]
pub(crate) fn private_mmap_eof_check_for_test() -> bool {
    matches!(
        cow_file_max_read_len(4096, None, 4096, 4096),
        Err(AxError::BadAddress)
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
    size: PageSize,
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

    fn alloc_new_frame(&self, zeroed: bool) -> AxResult<PhysAddr> {
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
        pt: &mut PageTableCursor,
    ) -> AxResult {
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
                return Err(err);
            }
        }
        let pte_flags = self.pte_flags_for_fault_in(flags, access_flags);
        if let Err(err) = pt.map(vaddr, frame, self.size, pte_flags) {
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
        pt: &mut PageTableCursor,
    ) -> AxResult<usize> {
        let Some((file, file_vaddr_base, file_start, file_end)) = &self.file else {
            for &addr in run {
                self.alloc_new_at(addr, flags, access_flags, acct, pt)?;
            }
            return Ok(run.len());
        };
        let ps = self.size as usize;
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
            if let Err(err) = pt.map(addr, frame, self.size, pte_flags) {
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
        pt: &mut PageTableCursor,
    ) -> AxResult {
        let mut frame_table = FRAME_TABLE.lock();
        let frame = frame_table
            .get_frame_ref(paddr)
            .ok_or(AxError::BadAddress)?;
        drop(frame_table);
        let mut frame = frame.lock();
        assert!(frame.count > 0, "invalid frame reference count");
        debug_assert!(frame.count < u8::MAX, "frame reference count near overflow");
        match frame.count {
            1 => {
                pt.protect(vaddr, vma_flags)?;
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
                if let Err(err) = pt.remap(vaddr, new_frame, vma_flags) {
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
        pt: &mut PageTableCursor,
    ) -> AxResult {
        if let Ok((frame, _flags, page_size)) = pt.unmap(addr) {
            assert_eq!(page_size, self.size);
            if let Some(acct) = acct {
                acct.remove_charge(addr);
            }
            let frame_ref = FRAME_TABLE
                .lock()
                .get_frame_ref(frame)
                .ok_or(AxError::BadAddress)?;
            let mut frame_ref = frame_ref.lock();
            frame_ref.drop_frame(frame, self.size);
        }
        Ok(())
    }

    pub fn file_info(&self) -> AxResult<BackendFileInfo> {
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
        if self.file.is_some() && flags.contains(MappingFlags::WRITE) {
            self.write_upgraded.set(true);
        }
        Ok(())
    }

    fn on_protect(
        &self,
        _range: VirtAddrRange,
        new_flags: MappingFlags,
        _pt: &mut PageTableCursor,
    ) -> AxResult {
        if self.file.is_some() && new_flags.contains(MappingFlags::WRITE) {
            self.write_upgraded.set(true);
        }
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
        _new_aspace: &Arc<PiMutex<AddrSpace>>,
        acct: CloneMapAccounting<'_>,
    ) -> AxResult<Backend> {
        let cow_flags = flags - MappingFlags::WRITE;

        for vaddr in pages_in(range, self.size)? {
            match old_pt.query(vaddr) {
                Ok((paddr, _pte_flags, page_size)) => {
                    assert_eq!(page_size, self.size);
                    let frame = FRAME_TABLE
                        .lock()
                        .get_frame_ref(paddr)
                        .ok_or(AxError::BadAddress)?;
                    let mut frame = frame.lock();
                    assert!(frame.count > 0, "referencing unreferenced frame");
                    frame.count += 1;
                    if frame.count == u8::MAX {
                        warn!("frame reference count overflow");
                        return Err(AxError::BadAddress);
                    }
                    old_pt.protect(vaddr, cow_flags)?;
                    new_pt.map(vaddr, paddr, self.size, cow_flags)?;
                    if let (Some(parent), Some(child)) = (acct.parent, acct.child)
                        && let Some(_kind) = parent.charge_kind(vaddr)
                    {
                        child.copy_charge_from(parent, vaddr)?;
                    }
                }
                Err(PagingError::NotMapped) => {}
                Err(_) => return Err(AxError::BadAddress),
            };
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

    fn shrink_left(&mut self, shrink_size: usize) {
        assert!(shrink_size.is_multiple_of(PAGE_SIZE_4K));
        self.start += shrink_size;
    }

    fn shrink_right(&mut self, _shrink_size: usize) {}
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
            file: Some((file, start, file_start, file_end)),
            name: None,
            shared,
            write_upgraded: Cell::new(false),
        })
    }

    pub fn new_alloc(start: VirtAddr, size: PageSize, name: &str) -> Self {
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
