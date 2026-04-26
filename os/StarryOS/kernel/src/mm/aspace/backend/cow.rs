use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
};
use core::slice;

use ax_errno::{AxError, AxResult};
use ax_fs::FileBackend;
use ax_hal::{
    mem::phys_to_virt,
    paging::{MappingFlags, PageSize, PageTableCursor, PagingError},
};
use ax_kspin::SpinNoIrq;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange, align_down_4k};
use ax_sync::Mutex;

use super::{
    AddrSpace, Backend, BackendOps, PopulateCallback, alloc_frame, dealloc_frame, pages_in,
};

struct FrameRefCnt(u8);

impl FrameRefCnt {
    // This function may lock FRAME_TABLE again, so the caller should drop the lock first.
    fn drop_frame(&mut self, paddr: PhysAddr, page_size: PageSize) {
        assert!(self.0 > 0, "dropping unreferenced frame");
        self.0 -= 1;
        if self.0 == 0 {
            // Remove the frame from FRAME_TABLE before deallocating it to avoid a race:
            // if we dealloc the frame first, another thread could allocate the same
            // physical frame before we remove the table entry. This function assumes
            // the caller is not holding the FRAME_TABLE lock, so it is safe to lock
            // FRAME_TABLE here and perform the removal.
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
            Arc::new(SpinNoIrq::new(FrameRefCnt(Self::INITIAL_CNT))),
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

/// Copy-on-write mapping backend.
///
/// This corresponds to the `MAP_PRIVATE` flag.
#[derive(Clone)]
pub struct CowBackend {
    start: VirtAddr,
    size: PageSize,
    file: Option<(FileBackend, u64, Option<u64>)>,
    name: Option<String>,
    shared: bool,
}

impl CowBackend {
    fn alloc_new_frame(&self, zeroed: bool) -> AxResult<PhysAddr> {
        let frame = alloc_frame(zeroed, self.size)?;
        FRAME_TABLE.lock().init_frame(frame);
        Ok(frame)
    }

    fn alloc_new_at(
        &self,
        vaddr: VirtAddr,
        flags: MappingFlags,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        let frame = self.alloc_new_frame(true)?;

        if let Some((file, file_start, file_end)) = &self.file {
            let buf = unsafe {
                slice::from_raw_parts_mut(phys_to_virt(frame).as_mut_ptr(), self.size as _)
            };
            // vaddr can be smaller than self.start (at most 1 page) due to
            // non-aligned mappings, we need to keep the gap clean.
            let start = self.start.as_usize().saturating_sub(vaddr.as_usize());
            assert!(start < self.size as _);

            let file_start =
                *file_start + vaddr.as_usize().saturating_sub(self.start.as_usize()) as u64;
            let max_read = file_end
                .map_or(u64::MAX, |end| end.saturating_sub(file_start))
                .min((buf.len() - start) as u64) as usize;

            file.read_at(&mut &mut buf[start..start + max_read], file_start)?;
        }
        pt.map(vaddr, frame, self.size, flags)?;
        Ok(())
    }

    fn handle_cow_fault(
        &self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        flags: MappingFlags,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        let mut frame_table = FRAME_TABLE.lock();
        let frame = frame_table
            .get_frame_ref(paddr)
            .ok_or(AxError::BadAddress)?;
        drop(frame_table);
        let mut frame = frame.lock();
        assert!(frame.0 > 0, "invalid frame reference count");
        match frame.0 {
            1 => {
                // Only one reference, just upgrade the permissions.
                pt.protect(vaddr, flags)?;
                return Ok(());
            }
            _ => {
                // Multiple references, need to copy the frame.
                let new_frame = self.alloc_new_frame(false)?;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        phys_to_virt(paddr).as_ptr(),
                        phys_to_virt(new_frame).as_mut_ptr(),
                        self.size as _,
                    );
                }
                pt.remap(vaddr, new_frame, flags)?;
                frame.drop_frame(paddr, self.size);
            }
        }

        Ok(())
    }

    pub fn file_info(&self) -> AxResult<(String, Option<u64>, Option<u64>, bool)> {
        let loc = self
            .file
            .as_ref()
            .map(|(file, offset, ..)| (file.location(), *offset));
        if let Some((loc, offset)) = loc {
            let path = loc.absolute_path().map(|pb| pb.to_string())?;
            let inode = loc.inode();
            let offset = align_down_4k(offset as usize) as u64;
            return Ok((path, Some(offset), Some(inode), self.shared));
        }
        if let Some(name) = &self.name {
            return Ok((name.clone(), None, None, self.shared));
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
        _pt: &mut PageTableCursor,
    ) -> AxResult {
        debug!("Cow::map: {range:?} {flags:?}",);
        Ok(())
    }

    fn unmap(&self, range: VirtAddrRange, pt: &mut PageTableCursor) -> AxResult {
        debug!("Cow::unmap: {range:?}");
        for addr in pages_in(range, self.size)? {
            if let Ok((frame, _flags, page_size)) = pt.unmap(addr) {
                assert_eq!(page_size, self.size);
                let frame_ref = FRAME_TABLE
                    .lock()
                    .get_frame_ref(frame)
                    .ok_or(AxError::BadAddress)?;
                let mut frame_ref = frame_ref.lock();
                frame_ref.drop_frame(frame, self.size);
            } else {
                // Deallocation is needn't if the page is not allocated.
            }
        }
        Ok(())
    }

    fn populate(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        access_flags: MappingFlags,
        pt: &mut PageTableCursor,
    ) -> AxResult<(usize, Option<PopulateCallback>)> {
        let mut pages = 0;
        for addr in pages_in(range, self.size)? {
            match pt.query(addr) {
                Ok((paddr, page_flags, page_size)) => {
                    assert_eq!(self.size, page_size);
                    if access_flags.contains(MappingFlags::WRITE)
                        && !page_flags.contains(MappingFlags::WRITE)
                    {
                        self.handle_cow_fault(addr, paddr, flags, pt)?;
                        pages += 1;
                    } else if page_flags.contains(access_flags) {
                        pages += 1;
                    }
                }
                // If the page is not mapped, try map it.
                Err(PagingError::NotMapped) => {
                    self.alloc_new_at(addr, flags, pt)?;
                    pages += 1;
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
    ) -> AxResult<Backend> {
        let cow_flags = flags - MappingFlags::WRITE;

        for vaddr in pages_in(range, self.size)? {
            // Copy data from old memory area to new memory area.
            match old_pt.query(vaddr) {
                Ok((paddr, _, page_size)) => {
                    assert_eq!(page_size, self.size);
                    // If the page is mapped in the old page table:
                    // - Update its permissions in the old page table using `flags`.
                    // - Map the same physical page into the new page table at the same
                    // virtual address, with the same page size and `flags`.
                    let frame = FRAME_TABLE
                        .lock()
                        .get_frame_ref(paddr)
                        .ok_or(AxError::BadAddress)?;
                    let mut frame = frame.lock();
                    assert!(frame.0 > 0, "referencing unreferenced frame");
                    frame.0 += 1;
                    if frame.0 == u8::MAX {
                        warn!("frame reference count overflow");
                        return Err(AxError::BadAddress);
                    }
                    old_pt.protect(vaddr, cow_flags)?;
                    new_pt.map(vaddr, paddr, self.size, cow_flags)?;
                }
                // If the page is not mapped, skip it.
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

        if let Some((_, file_start, _)) = right.file.as_mut() {
            *file_start += align_diff as u64;
        }
        Some(Backend::Cow(right))
    }

    fn shrink_left(&mut self, shrink_size: usize) {
        assert!(shrink_size.is_multiple_of(PAGE_SIZE_4K));
        self.start += shrink_size;
        self.file.as_mut().map(|(_, file_start, _)| {
            *file_start += shrink_size as u64;
            Some(())
        });
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
            start,
            size,
            file: Some((file, file_start, file_end)),
            name: None,
            shared,
        })
    }

    pub fn new_alloc(start: VirtAddr, size: PageSize, name: &str) -> Self {
        Self::Cow(CowBackend {
            start,
            size,
            file: None,
            name: Some(name.to_string()),
            shared: false,
        })
    }
}
