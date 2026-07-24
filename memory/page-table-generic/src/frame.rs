//! Page-table frame ownership and entry access.

use super::{
    PageFrameProvider, PageTableEntry, PagingError, PagingResult, PhysAddr, PteConfig, TableMeta,
    VirtAddr,
};

/// View over one or more contiguous page-table frames.
#[derive(Clone, Copy)]
pub struct Frame<T: TableMeta, A: PageFrameProvider> {
    /// Physical start address of the frame range.
    pub paddr: PhysAddr,
    /// Provider used to translate and release the frame range.
    pub allocator: A,
    frames: usize,
    _marker: core::marker::PhantomData<T>,
}

impl<T: TableMeta, A: PageFrameProvider> core::fmt::Debug for Frame<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Frame")
            .field("paddr", &format_args!("{:#x}", self.paddr.as_usize()))
            .finish()
    }
}

impl<T, A> Frame<T, A>
where
    T: TableMeta,
    A: PageFrameProvider,
{
    pub(crate) const PT_INDEX_SHIFT: usize = T::PAGE_SIZE.trailing_zeros() as usize;
    pub(crate) const PT_INDEX_BITS: usize = {
        let mut bits = 0;
        let mut index = 0;
        while index < T::LEVEL_BITS.len() {
            bits += T::LEVEL_BITS[index];
            index += 1;
        }
        bits
    };
    pub(crate) const PT_VALID_BITS: usize = Self::PT_INDEX_BITS + Self::PT_INDEX_SHIFT;
    pub(crate) const LEN: usize = T::PAGE_SIZE / core::mem::size_of::<T::P>();
    pub(crate) const ROOT_LEN: usize = 1usize << T::LEVEL_BITS[0];
    pub(crate) const ROOT_FRAMES: usize =
        (Self::ROOT_LEN * core::mem::size_of::<T::P>()).div_ceil(T::PAGE_SIZE);
    pub(crate) const PT_LEVEL: usize = T::LEVEL_BITS.len();

    /// Allocates and zeroes one page-table frame.
    pub(crate) fn new(allocator: A) -> PagingResult<Self> {
        Self::validate_provider_frame_size()?;
        let paddr = allocator.alloc_frame().ok_or(PagingError::NoMemory)?;
        let vaddr = allocator.phys_to_virt(paddr);
        // SAFETY: the provider returned an exclusively owned frame of exactly
        // `T::PAGE_SIZE` bytes, and the address remains mapped for its lifetime.
        unsafe {
            core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, T::PAGE_SIZE);
        }

        Ok(Self {
            paddr,
            allocator,
            frames: 1,
            _marker: core::marker::PhantomData,
        })
    }

    /// Allocates and zeroes the contiguous root-table frame range.
    pub(crate) fn new_root(allocator: A) -> PagingResult<Self> {
        Self::validate_provider_frame_size()?;
        let align = T::PAGE_SIZE * Self::ROOT_FRAMES;
        let paddr = allocator
            .alloc_frames(Self::ROOT_FRAMES, align)
            .ok_or(PagingError::NoMemory)?;
        let vaddr = allocator.phys_to_virt(paddr);
        // SAFETY: `alloc_frames` returned `ROOT_FRAMES` exclusive contiguous
        // frames and the provider keeps the translated range mapped.
        unsafe {
            core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, T::PAGE_SIZE * Self::ROOT_FRAMES);
        }

        Ok(Self {
            paddr,
            allocator,
            frames: Self::ROOT_FRAMES,
            _marker: core::marker::PhantomData,
        })
    }

    /// Creates a non-owning single-frame view from a physical address.
    pub(crate) fn from_paddr(paddr: PhysAddr, allocator: A) -> Self {
        Self {
            paddr,
            allocator,
            frames: 1,
            _marker: core::marker::PhantomData,
        }
    }

    /// Creates a non-owning root-frame view from a physical address.
    pub(crate) fn from_root_paddr(paddr: PhysAddr, allocator: A) -> Self {
        Self {
            paddr,
            allocator,
            frames: Self::ROOT_FRAMES,
            _marker: core::marker::PhantomData,
        }
    }

    /// Creates a non-owning child-frame view from a directory entry.
    pub(crate) fn from_pte(pte: &T::P, level: usize, allocator: A) -> Self {
        let config = pte.to_config(level > 1);
        Self::from_paddr(config.paddr, allocator)
    }

    /// Returns mutable access to the entries in this frame range.
    pub(crate) fn as_slice_mut(&mut self) -> &mut [T::P] {
        let vaddr = self.allocator.phys_to_virt(self.paddr);
        // SAFETY: `&mut self` provides exclusive access to the frame view; the
        // provider contract keeps the complete frame range mapped and aligned.
        unsafe { core::slice::from_raw_parts_mut(vaddr.as_mut_ptr() as *mut T::P, self.len()) }
    }

    /// Returns shared access to the entries in this frame range.
    pub(crate) fn as_slice(&self) -> &[T::P] {
        let vaddr = self.allocator.phys_to_virt(self.paddr);
        // SAFETY: the provider contract keeps the complete frame range mapped
        // and aligned, and the returned slice is bounded by `&self`.
        unsafe { core::slice::from_raw_parts(vaddr.as_ptr() as *const T::P, self.len()) }
    }

    /// Returns the number of entries in this frame range.
    pub(crate) fn len(&self) -> usize {
        self.frames * Self::LEN
    }

    fn validate_provider_frame_size() -> PagingResult {
        if A::FRAME_SIZE == T::PAGE_SIZE {
            Ok(())
        } else {
            Err(PagingError::InvalidSize {
                details: "page-table size does not match provider frame size",
            })
        }
    }

    /// Returns the mapping size represented by an internal table level.
    pub(crate) fn level_size(level: usize) -> usize {
        super::level_size::<T>(level).expect("internal page-table level must be valid")
    }

    /// Extracts the index for one level from a virtual address.
    pub(crate) fn virt_to_index(vaddr: VirtAddr, level: usize) -> usize {
        if level == 0 || level > Self::PT_LEVEL {
            panic!("Invalid level: {} (valid: 1..={})", level, Self::PT_LEVEL);
        }

        let page_shift = T::PAGE_SIZE.trailing_zeros() as usize;
        let total_levels = T::LEVEL_BITS.len();

        let shift = if level == 1 {
            page_shift
        } else {
            page_shift
                + T::LEVEL_BITS
                    .iter()
                    .skip(total_levels - level + 1)
                    .sum::<usize>()
        };

        let level_index_bits = T::LEVEL_BITS[total_levels - level];
        let mask = (1 << level_index_bits) - 1;

        (vaddr.as_usize() >> shift) & mask
    }

    /// Reconstructs an entry virtual address from its level base and index.
    pub(crate) fn reconstruct_vaddr(index: usize, level: usize, base_vaddr: VirtAddr) -> VirtAddr {
        let entry_size = Self::level_size(level);
        base_vaddr + index * entry_size
    }

    /// Recursively releases this frame and all child table frames.
    ///
    /// Mapped data frames are not released. The caller must ensure no CPU or
    /// alias can access the table during or after this operation.
    pub(crate) fn deallocate_recursive(&mut self, level: usize) {
        self.deallocate_children(level);
        self.allocator.dealloc_frames(self.paddr, self.frames);
    }

    /// Releases child table frames while preserving leaf and block mappings.
    fn deallocate_children(&mut self, level: usize) {
        for i in (0..self.len()).rev() {
            let entry_info = {
                let entries = self.as_slice();
                if i < entries.len() {
                    let config = entries[i].to_config(level > 1);
                    (config.valid, config.huge, config.paddr)
                } else {
                    (false, false, crate::PhysAddr::from_usize(0))
                }
            };

            let (is_valid, is_huge, paddr) = entry_info;

            if !is_valid {
                continue;
            }

            if is_huge || level == 1 {
                continue;
            } else {
                let mut child_frame = Frame::<T, A>::from_paddr(paddr, self.allocator.clone());
                child_frame.deallocate_recursive(level - 1);

                let entries_mut = self.as_slice_mut();
                let invalid_config = PteConfig {
                    valid: false,
                    ..Default::default()
                };
                entries_mut[i] = T::P::from_config(invalid_config);
            }
        }
    }

    /// Descends to the entry covering `vaddr` and returns its level.
    pub(crate) fn translate_recursive_with_level(
        &self,
        vaddr: VirtAddr,
        level: usize,
    ) -> PagingResult<(T::P, usize)> {
        let index = Self::virt_to_index(vaddr, level);
        let entries = self.as_slice();
        let pte = entries[index];
        let config = pte.to_config(level > 1);
        if !config.valid {
            return Err(PagingError::NotMapped);
        }

        if config.huge || level == 1 {
            return Ok((pte, level));
        }

        if level > 1 {
            let child_frame: Frame<T, A> = Frame::from_pte(&pte, level, self.allocator.clone());
            return child_frame.translate_recursive_with_level(vaddr, level - 1);
        }

        Err(PagingError::HierarchyError {
            details: "Invalid page table level during translation",
        })
    }
}
