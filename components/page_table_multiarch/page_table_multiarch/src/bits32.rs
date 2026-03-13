use core::{marker::PhantomData, ops::Deref};

use arrayvec::ArrayVec;
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr};

use crate::{
    GenericPTE, MappingFlags, PageSize, PagingError, PagingHandler, PagingMetaData, PagingResult,
    TlbFlusher,
};

#[cfg(target_arch = "arm")]
const ENTRY_COUNT: usize = 4096; // ARMv7-A L1 has 4096 entries
#[cfg(not(target_arch = "arm"))]
const ENTRY_COUNT: usize = 512; // 512 entries per table

/// Extract the L1 (first-level) page table index from a virtual address.
///
/// For ARMv7-A:
/// - L1 uses bits[31:20] of the virtual address (12 bits = 4096 entries)
/// - Each L1 entry covers 1MB of virtual address space
const fn p1_index(vaddr: usize) -> usize {
    (vaddr >> 20) & 0xFFF // bits[31:20] for 1MB sections
}

/// Extract the L2 (second-level) page table index from a virtual address.
///
/// For ARMv7-A:
/// - L2 uses bits[19:12] of the virtual address (8 bits = 256 entries)
/// - Each L2 entry covers 4KB of virtual address space
const fn p2_index(vaddr: usize) -> usize {
    (vaddr >> 12) & 0xFF // bits[19:12] for 4KB pages
}

/// A generic page table struct for 32-bit ARM platform (ARMv7-A).
///
/// This implements a 2-level page table:
/// - L1: 4096 entries, each covering 1MB (Section) or pointing to L2
/// - L2: 256 entries, each covering 4KB (Small Page)
///
/// It tracks all L2 tables for proper deallocation.
pub struct PageTable32<M: PagingMetaData, PTE: GenericPTE, H: PagingHandler> {
    root_paddr: PhysAddr,
    #[cfg(feature = "copy-from")]
    borrowed_entries: [u64; ENTRY_COUNT / 64],
    _phantom: PhantomData<(M, PTE, H)>,
}

impl<M: PagingMetaData, PTE: GenericPTE, H: PagingHandler> PageTable32<M, PTE, H> {
    /// Creates a new page table instance or returns the error.
    ///
    /// It will allocate a new 16KB aligned page for the L1 page table.
    pub fn try_new() -> PagingResult<Self> {
        let (root_paddr, size_pages) = {
            #[cfg(target_arch = "arm")]
            {
                // ARMv7-A L1 page table: 4096 entries * 4 bytes = 16KB
                // Requires 16KB alignment for TTBR0
                const L1_SIZE_PAGES: usize = 4; // 16KB = 4 * 4KB
                const L1_ALIGN: usize = 16384; // 16KB alignment

                let root_paddr =
                    H::alloc_frames(L1_SIZE_PAGES, L1_ALIGN).ok_or(PagingError::NoMemory)?;

                (root_paddr, L1_SIZE_PAGES)
            }

            #[cfg(not(target_arch = "arm"))]
            {
                // Other 32-bit architectures page table: 512 entries * 8 bytes = 4KB
                const SIZE_PAGES: usize = 1; // 4KB = 1 * 4KB
                let root_paddr = H::alloc_frame().ok_or(PagingError::NoMemory)?;
                (root_paddr, SIZE_PAGES)
            }
        };

        // Zero out the root page table
        let virt = H::phys_to_virt(root_paddr);
        unsafe {
            core::ptr::write_bytes(virt.as_mut_ptr(), 0, size_pages * PAGE_SIZE_4K);
        }

        Ok(Self {
            root_paddr,
            #[cfg(feature = "copy-from")]
            borrowed_entries: [0; ENTRY_COUNT / 64],
            _phantom: PhantomData,
        })
    }

    /// Returns the physical address of the root page table (L1).
    pub const fn root_paddr(&self) -> PhysAddr {
        self.root_paddr
    }

    /// Query the result of the mapping starts with `vaddr`.
    ///
    /// Returns the physical address of the target frame, the mapping flags, and
    /// the page size.
    ///
    /// Returns [`Err(PagingError::NotMapped)`](PagingError::NotMapped) if the
    /// mapping is not present.
    pub fn query(&self, vaddr: M::VirtAddr) -> PagingResult<(PhysAddr, MappingFlags, PageSize)> {
        let (entry, size) = self.get_entry(vaddr)?;
        if entry.is_unused() {
            return Err(PagingError::NotMapped);
        }
        let off = vaddr.into() & (size as usize - 1);
        Ok((entry.paddr().add(off), entry.flags(), size))
    }

    /// Walk the page table recursively.
    pub fn walk<F>(&self, limit: usize, pre_func: Option<&F>, post_func: Option<&F>)
    where
        F: Fn(usize, usize, M::VirtAddr, &PTE),
    {
        self.walk_recursive(
            self.get_table(self.root_paddr),
            0,
            0.into(),
            limit,
            pre_func,
            post_func,
        )
    }

    /// Gets a cursor to modify the page table.
    ///
    /// The TLB will be flushed automatically when the cursor is dropped.
    pub fn cursor(&mut self) -> PageTable32Cursor<'_, M, PTE, H> {
        PageTable32Cursor::new(self)
    }

    // Private helpers
    fn get_entry_mut(&mut self, vaddr: M::VirtAddr) -> PagingResult<(&mut PTE, PageSize)> {
        let vaddr_usize = vaddr.into();
        let p1 = p1_index(vaddr_usize);
        let table = self.get_table_mut(self.root_paddr);
        let entry = &mut table[p1];

        if entry.is_unused() {
            return Err(PagingError::NotMapped);
        }

        // Check if it's a 1MB Section
        if entry.is_huge() {
            return Ok((entry, PageSize::Size1M));
        }

        // It's a page table pointer, go to L2
        let p2_table = self.get_table_mut(entry.paddr());
        let p2 = p2_index(vaddr_usize);
        Ok((&mut p2_table[p2], PageSize::Size4K))
    }

    fn get_entry(&self, vaddr: M::VirtAddr) -> PagingResult<(&PTE, PageSize)> {
        let vaddr_usize = vaddr.into();
        let p1 = p1_index(vaddr_usize);
        let table = self.get_table(self.root_paddr);
        let entry = &table[p1];

        if entry.is_unused() {
            return Err(PagingError::NotMapped);
        }

        if entry.is_huge() {
            return Ok((entry, PageSize::Size1M));
        }

        let p2_table = self.get_table(entry.paddr());
        let p2 = p2_index(vaddr_usize);
        Ok((&p2_table[p2], PageSize::Size4K))
    }

    fn get_entry_mut_or_create(
        &mut self,
        vaddr: M::VirtAddr,
        page_size: PageSize,
    ) -> PagingResult<&mut PTE> {
        let vaddr_usize = vaddr.into();
        let p1 = p1_index(vaddr_usize);
        let table = self.get_table_mut(self.root_paddr);

        if page_size == PageSize::Size1M {
            // Map as 1MB Section in L1
            return Ok(&mut table[p1]);
        }

        // Need L2 page table for 4KB mapping
        let entry = &mut table[p1];
        if entry.is_unused() {
            // Create new L2 page table (allocate 4KB, though only 1KB is used)
            let paddr = H::alloc_frame().ok_or(PagingError::NoMemory)?;

            // Zero out the L2 page table
            let virt = H::phys_to_virt(paddr);
            unsafe {
                core::ptr::write_bytes(virt.as_mut_ptr(), 0, PAGE_SIZE_4K);
            }

            *entry = GenericPTE::new_table(paddr);
        } else if entry.is_huge() {
            // Already mapped as huge page
            return Err(PagingError::AlreadyMapped);
        }

        let p2_table = self.get_table_mut(entry.paddr());
        let p2 = p2_index(vaddr_usize);
        Ok(&mut p2_table[p2])
    }

    fn get_table<'a>(&self, paddr: PhysAddr) -> &'a [PTE] {
        let ptr = H::phys_to_virt(paddr).as_ptr() as *const PTE;
        unsafe { core::slice::from_raw_parts(ptr, ENTRY_COUNT) }
    }

    fn get_table_mut<'a>(&self, paddr: PhysAddr) -> &'a mut [PTE] {
        let ptr = H::phys_to_virt(paddr).as_mut_ptr() as *mut PTE;
        unsafe { core::slice::from_raw_parts_mut(ptr, ENTRY_COUNT) }
    }

    fn walk_recursive<F>(
        &self,
        table: &[PTE],
        level: usize,
        start_vaddr: M::VirtAddr,
        limit: usize,
        pre_func: Option<&F>,
        post_func: Option<&F>,
    ) where
        F: Fn(usize, usize, M::VirtAddr, &PTE),
    {
        let start_vaddr_usize: usize = start_vaddr.into();
        let mut n = 0;
        for (i, entry) in table.iter().enumerate() {
            // L1 (level 0): each entry covers 1MB (shift 20)
            // L2 (level 1): each entry covers 4KB (shift 12)
            let shift = if level == 0 { 20 } else { 12 };
            let vaddr_usize = start_vaddr_usize + (i << shift);
            let vaddr = vaddr_usize.into();

            if !entry.is_unused() {
                if let Some(func) = pre_func {
                    func(level, i, vaddr, entry);
                }
                if level == 0 && !entry.is_huge() {
                    let next_table = self.get_table(entry.paddr());
                    self.walk_recursive(next_table, level + 1, vaddr, limit, pre_func, post_func);
                }
                if let Some(func) = post_func {
                    func(level, i, vaddr, entry);
                }
                n += 1;
                if n >= limit {
                    break;
                }
            }
        }
    }
}

impl<M: PagingMetaData, PTE: GenericPTE, H: PagingHandler> Drop for PageTable32<M, PTE, H> {
    fn drop(&mut self) {
        // Deallocate all L2 page tables (each is 4KB)
        let table = self.get_table(self.root_paddr);
        #[allow(unused_variables)]
        for (i, entry) in table.iter().enumerate() {
            #[cfg(feature = "copy-from")]
            if (self.borrowed_entries[i / 64] & (1 << (i % 64))) != 0 {
                continue;
            }
            if !entry.is_unused() && !entry.is_huge() {
                // This is an L2 page table (4KB)
                H::dealloc_frame(entry.paddr());
            }
        }
        // Deallocate L1 page table (16KB = 4 pages)
        H::dealloc_frames(self.root_paddr, 4);
    }
}

/// A cursor created by [`PageTable32::cursor`] to modify the page table.
pub struct PageTable32Cursor<'a, M: PagingMetaData, PTE: GenericPTE, H: PagingHandler> {
    inner: &'a mut PageTable32<M, PTE, H>,
    flusher: TlbFlusher<M>,
}

impl<M: PagingMetaData, PTE: GenericPTE, H: PagingHandler> Deref
    for PageTable32Cursor<'_, M, PTE, H>
{
    type Target = PageTable32<M, PTE, H>;

    fn deref(&self) -> &PageTable32<M, PTE, H> {
        self.inner
    }
}

impl<'a, M: PagingMetaData, PTE: GenericPTE, H: PagingHandler> PageTable32Cursor<'a, M, PTE, H> {
    fn new(inner: &'a mut PageTable32<M, PTE, H>) -> Self {
        Self {
            inner,
            flusher: TlbFlusher::None,
        }
    }

    fn push(&mut self, vaddr: M::VirtAddr) {
        match self.flusher {
            TlbFlusher::None => {
                let mut arr = ArrayVec::new();
                arr.push(vaddr);
                self.flusher = TlbFlusher::Array(arr);
            }
            TlbFlusher::Array(ref mut arr) => {
                if arr.try_push(vaddr).is_err() {
                    self.flusher = TlbFlusher::Full;
                }
            }
            TlbFlusher::Full => {}
        }
    }

    /// Maps a virtual page to a physical frame with the given `page_size`
    /// and mapping `flags`.
    pub fn map(
        &mut self,
        vaddr: M::VirtAddr,
        target: PhysAddr,
        page_size: PageSize,
        flags: MappingFlags,
    ) -> PagingResult {
        let entry = self.inner.get_entry_mut_or_create(vaddr, page_size)?;
        if !entry.is_unused() {
            return Err(PagingError::AlreadyMapped);
        }
        *entry = GenericPTE::new_page(target.align_down(page_size), flags, page_size.is_huge());
        self.push(vaddr);
        Ok(())
    }

    /// Remaps the mapping starting at `vaddr`, updates both the physical
    /// address and flags.
    pub fn remap(
        &mut self,
        vaddr: M::VirtAddr,
        paddr: PhysAddr,
        flags: MappingFlags,
    ) -> PagingResult<PageSize> {
        let (entry, size) = self.inner.get_entry_mut(vaddr)?;
        *entry = GenericPTE::new_page(paddr, flags, size.is_huge());
        self.push(vaddr);
        Ok(size)
    }

    /// Updates the flags of the mapping starting at `vaddr`.
    pub fn protect(&mut self, vaddr: M::VirtAddr, flags: MappingFlags) -> PagingResult<PageSize> {
        let (entry, size) = self.inner.get_entry_mut(vaddr)?;
        if entry.is_unused() {
            return Err(PagingError::NotMapped);
        }
        *entry = GenericPTE::new_page(entry.paddr(), flags, size.is_huge());
        self.push(vaddr);
        Ok(size)
    }

    /// Unmaps the mapping starting at `vaddr`.
    pub fn unmap(
        &mut self,
        vaddr: M::VirtAddr,
    ) -> PagingResult<(PhysAddr, MappingFlags, PageSize)> {
        let (entry, size) = self.inner.get_entry_mut(vaddr)?;
        if entry.is_unused() {
            return Err(PagingError::NotMapped);
        }
        let paddr = entry.paddr();
        let flags = entry.flags();
        entry.clear();
        self.push(vaddr);
        Ok((paddr, flags, size))
    }

    /// Maps a contiguous virtual memory region to a contiguous physical memory
    /// region with the given mapping `flags`.
    pub fn map_region(
        &mut self,
        vaddr: M::VirtAddr,
        get_paddr: impl Fn(M::VirtAddr) -> PhysAddr,
        size: usize,
        flags: MappingFlags,
        allow_huge: bool,
    ) -> PagingResult {
        let mut vaddr_usize: usize = vaddr.into();
        let mut size = size;
        if !PageSize::Size4K.is_aligned(vaddr_usize) || !PageSize::Size4K.is_aligned(size) {
            return Err(PagingError::NotAligned);
        }
        trace!(
            "map_region({:#x}): [{:#x}, {:#x}) {:?}",
            self.root_paddr(),
            vaddr_usize,
            vaddr_usize + size,
            flags,
        );
        while size > 0 {
            let vaddr = vaddr_usize.into();
            let paddr = get_paddr(vaddr);
            let page_size = if allow_huge
                && PageSize::Size1M.is_aligned(vaddr_usize)
                && paddr.is_aligned(PageSize::Size1M)
                && size >= PageSize::Size1M as usize
            {
                PageSize::Size1M
            } else {
                PageSize::Size4K
            };
            self.map(vaddr, paddr, page_size, flags).inspect_err(|e| {
                error!("failed to map page: {vaddr_usize:#x?}({page_size:?}) -> {paddr:#x?}, {e:?}")
            })?;

            vaddr_usize += page_size as usize;
            size -= page_size as usize;
        }
        Ok(())
    }

    /// Unmaps a contiguous virtual memory region.
    pub fn unmap_region(&mut self, vaddr: M::VirtAddr, size: usize) -> PagingResult {
        let mut vaddr_usize: usize = vaddr.into();
        let mut size = size;
        trace!(
            "unmap_region({:#x}) [{:#x}, {:#x})",
            self.root_paddr(),
            vaddr_usize,
            vaddr_usize + size,
        );
        while size > 0 {
            let vaddr = vaddr_usize.into();
            let (_, _, page_size) = self
                .unmap(vaddr)
                .inspect_err(|e| error!("failed to unmap page: {vaddr_usize:#x?}, {e:?}"))?;

            assert!(page_size.is_aligned(vaddr_usize));
            assert!(page_size as usize <= size);
            vaddr_usize += page_size as usize;
            size -= page_size as usize;
        }
        Ok(())
    }

    /// Updates mapping flags of a contiguous virtual memory region.
    pub fn protect_region(
        &mut self,
        vaddr: M::VirtAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult {
        let mut vaddr_usize: usize = vaddr.into();
        let mut size = size;
        trace!(
            "protect_region({:#x}) [{:#x}, {:#x}) {:?}",
            self.root_paddr(),
            vaddr_usize,
            vaddr_usize + size,
            flags,
        );
        while size > 0 {
            let vaddr = vaddr_usize.into();
            let page_size = match self.inner.get_entry_mut(vaddr) {
                Ok((entry, page_size)) => {
                    if !entry.is_unused() {
                        entry.set_flags(flags, page_size.is_huge());
                        self.push(vaddr);
                    }
                    // ignore if not present

                    page_size
                }
                Err(PagingError::NotMapped) => PageSize::Size4K,
                Err(e) => {
                    error!("failed to protect page: {vaddr_usize:#x?}, {e:?}");
                    return Err(e);
                }
            };

            assert!(page_size.is_aligned(vaddr_usize));
            assert!(page_size as usize <= size);
            vaddr_usize += page_size as usize;
            size -= page_size as usize;
        }
        Ok(())
    }

    /// Copy entries from another page table within the given virtual memory
    /// range.
    #[cfg(feature = "copy-from")]
    pub fn copy_from(&mut self, other: &PageTable32<M, PTE, H>, start: M::VirtAddr, size: usize) {
        if size == 0 {
            return;
        }
        let src_table = self.inner.get_table(other.root_paddr);
        let dst_table = self.inner.get_table_mut(self.inner.root_paddr);

        let start_idx = p1_index(start.into());
        let end_idx = p1_index(start.into() + size - 1) + 1;
        assert!(start_idx < ENTRY_COUNT);
        assert!(end_idx <= ENTRY_COUNT);

        // Simple copy here, no smart flush or tracking borrowing for now in 32-bit
        // The user just wants interface consistency.
        for i in start_idx..end_idx {
            let entry = &mut dst_table[i];
            let is_borrowed = (self.inner.borrowed_entries[i / 64] & (1 << (i % 64))) != 0;
            if !is_borrowed {
                self.inner.borrowed_entries[i / 64] |= 1 << (i % 64);
                if !entry.is_unused() && !entry.is_huge() {
                    H::dealloc_frame(entry.paddr());
                }
            }
            *entry = src_table[i];
        }
        self.flusher = TlbFlusher::Full;
    }

    /// Flushes the TLB according to the recorded flush requests.
    pub fn flush(&mut self) {
        #[cfg(not(docsrs))]
        match &self.flusher {
            TlbFlusher::None => {}
            TlbFlusher::Array(addrs) => {
                for vaddr in addrs.iter() {
                    M::flush_tlb(Some(*vaddr));
                }
            }
            TlbFlusher::Full => {
                M::flush_tlb(None);
            }
        }
        self.flusher = TlbFlusher::None;
    }
}

impl<M: PagingMetaData, PTE: GenericPTE, H: PagingHandler> Drop
    for PageTable32Cursor<'_, M, PTE, H>
{
    fn drop(&mut self) {
        self.flush();
    }
}
