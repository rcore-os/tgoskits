use core::ops::{Deref, DerefMut};

use crate::flexible::{
    PageFrameProvider, PageTableEntry, PagingError, PagingResult, PhysAddr, TableMeta, VirtAddr,
    frame::Frame,
    map::{MapConfig, MapRecursiveConfig, UnmapConfig, UnmapRecursiveConfig},
    walk::{PageTableWalker, WalkConfig},
};

/// Owning page table that releases only its table frames on drop.
pub struct PageTable<T: TableMeta, A: PageFrameProvider> {
    inner: PageTableRef<T, A>,
}

impl<T: TableMeta, A: PageFrameProvider> PageTable<T, A> {
    /// Number of virtual-address bits represented by the configured geometry.
    pub const VALID_BITS: usize = Frame::<T, A>::PT_VALID_BITS;

    /// Allocates an empty page table.
    pub fn new(allocator: A) -> PagingResult<Self> {
        // SAFETY: the newly allocated root is exclusively owned by `inner`.
        let inner = unsafe { PageTableRef::new(allocator) }?;
        Ok(Self { inner })
    }

    /// Returns the number of represented virtual-address bits.
    pub fn valid_bits(&self) -> usize {
        Frame::<T, A>::PT_VALID_BITS
    }
}

impl<T: TableMeta, A: PageFrameProvider> Drop for PageTable<T, A> {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: `PageTable` uniquely owns its root and drop runs once.
            self.deallocate();
        }
    }
}

impl<T: TableMeta, A: PageFrameProvider> Deref for PageTable<T, A> {
    type Target = PageTableRef<T, A>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: TableMeta, A: PageFrameProvider> DerefMut for PageTable<T, A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Non-dropping view over a page-table root.
///
/// Copies refer to the same table and therefore never release frames
/// automatically.
#[derive(Clone, Copy)]
pub struct PageTableRef<T: TableMeta, A: PageFrameProvider> {
    pub root: Frame<T, A>,
}

impl<T: TableMeta, A: PageFrameProvider> core::fmt::Debug for PageTableRef<T, A>
where
    T::P: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PageTable")
            .field(
                "root_paddr",
                &format_args!("{:#x}", self.root.paddr.as_usize()),
            )
            .field("table_levels", &T::LEVEL_BITS.len())
            .field("max_block_level", &T::MAX_BLOCK_LEVEL)
            .field("page_size", &format_args!("{:#x}", T::PAGE_SIZE))
            .finish()
    }
}

impl<T: TableMeta, A: PageFrameProvider> PageTableRef<T, A> {
    /// Allocates an empty root table.
    ///
    /// # Safety
    ///
    /// The provider must keep every allocated frame mapped and exclusively
    /// available to this page table until the table is destroyed.
    pub unsafe fn new(allocator: A) -> PagingResult<Self> {
        let root = Frame::new_root(allocator)?;
        Ok(Self { root })
    }

    /// Creates a non-owning view of an existing root table.
    pub fn from_paddr(paddr: PhysAddr, allocator: A) -> Self {
        let root = Frame::from_root_paddr(paddr, allocator);
        Self { root }
    }

    /// Maps one contiguous virtual range to a contiguous physical range.
    ///
    /// # Errors
    ///
    /// Returns a typed paging error for invalid ranges, address overflow,
    /// allocation failure, or an existing conflicting mapping.
    pub fn map(&mut self, config: &MapConfig) -> PagingResult {
        self.validate_map_config(config)?;

        if config.vaddr.as_usize().checked_add(config.size).is_none()
            || config.paddr.as_usize().checked_add(config.size).is_none()
        {
            return Err(PagingError::address_overflow(
                "Virtual or physical address overflow",
            ));
        }
        self.validate_address_width(config.vaddr, config.size, "map")?;

        self.root.map_range_recursive(MapRecursiveConfig {
            start_vaddr: config.vaddr,
            start_paddr: config.paddr,
            end_vaddr: config.vaddr + config.size,
            level: Frame::<T, A>::PT_LEVEL,
            allow_huge: config.allow_huge,
            flush: config.flush,
            pte_template: config.pte,
        })?;

        Ok(())
    }

    /// Unmaps one virtual range and reclaims empty intermediate tables.
    ///
    /// # Errors
    ///
    /// Returns a typed paging error for an invalid or overflowing range.
    pub fn unmap(&mut self, start_vaddr: VirtAddr, size: usize) -> PagingResult<()> {
        self.validate_unmap_params(start_vaddr, size)?;

        let end_vaddr: VirtAddr = match start_vaddr.as_usize().checked_add(size) {
            Some(end) => VirtAddr::from_usize(end),
            None => {
                return Err(PagingError::address_overflow(
                    "Virtual address overflow in unmap",
                ));
            }
        };
        self.validate_address_width(start_vaddr, size, "unmap")?;

        self.root.unmap_range_recursive(UnmapRecursiveConfig {
            start_vaddr,
            end_vaddr,
            level: Frame::<T, A>::PT_LEVEL,
            flush: true,
        })?;

        Ok(())
    }

    /// Unmaps a range using an explicit TLB-flush policy.
    pub fn unmap_with_config(&mut self, config: &UnmapConfig) -> PagingResult<()> {
        self.validate_unmap_params(config.start_vaddr, config.size)?;

        let end_vaddr = match config.start_vaddr.as_usize().checked_add(config.size) {
            Some(end) => VirtAddr::from_usize(end),
            None => {
                return Err(PagingError::address_overflow(
                    "Virtual address overflow in unmap_with_config",
                ));
            }
        };
        self.validate_address_width(config.start_vaddr, config.size, "unmap_with_config")?;

        self.root.unmap_range_recursive(UnmapRecursiveConfig {
            start_vaddr: config.start_vaddr,
            end_vaddr,
            level: Frame::<T, A>::PT_LEVEL,
            flush: config.flush,
        })?;

        Ok(())
    }

    fn validate_unmap_params(&self, start_vaddr: VirtAddr, size: usize) -> PagingResult<()> {
        if size == 0 {
            return Err(PagingError::invalid_size("Size cannot be zero in unmap"));
        }

        if !start_vaddr.as_usize().is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Start virtual address not page aligned in unmap",
            ));
        }

        if !size.is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Size not page aligned in unmap",
            ));
        }

        Ok(())
    }

    /// Walks every entry intersecting a virtual range.
    pub fn walk_all(&self, config: WalkConfig) -> PageTableWalker<'_, T, A> {
        PageTableWalker::new(self, config)
    }

    /// Walks valid entries intersecting a virtual range.
    pub fn walk(
        &self,
        start_vaddr: VirtAddr,
        end_vaddr: VirtAddr,
    ) -> impl Iterator<Item = crate::flexible::walk::PteInfo<T::P>> + '_ {
        let config = WalkConfig {
            start_vaddr,
            end_vaddr,
        };
        PageTableWalker::new(self, config).filter(|p| p.pte.to_config(false).valid)
    }

    /// Walks valid leaf and block mappings across the address space.
    pub fn walk_valid(&self) -> impl Iterator<Item = crate::flexible::walk::PteInfo<T::P>> + '_ {
        self.walk(0.into(), usize::MAX.into()).filter(|p| {
            let config = p.pte.to_config(false);
            config.valid && p.is_final_mapping
        })
    }

    fn validate_map_config(&self, config: &MapConfig) -> PagingResult {
        if config.size == 0 {
            return Err(PagingError::invalid_size("Size cannot be zero"));
        }

        if !config.vaddr.as_usize().is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Virtual address not page aligned",
            ));
        }

        if !config.paddr.as_usize().is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Physical address not page aligned",
            ));
        }

        Ok(())
    }

    fn validate_address_width(
        &self,
        start_vaddr: VirtAddr,
        size: usize,
        operation: &'static str,
    ) -> PagingResult<()> {
        if !T::STRICT_ADDRESS_WIDTH {
            return Ok(());
        }
        let Some(end) = start_vaddr.as_usize().checked_add(size) else {
            return Err(PagingError::address_overflow(
                "Virtual address range overflow",
            ));
        };
        let last = end.saturating_sub(1);
        if !Self::is_addr_in_width(start_vaddr.as_usize()) || !Self::is_addr_in_width(last) {
            return Err(PagingError::address_overflow(operation));
        }
        Ok(())
    }

    /// Returns the base page size.
    pub const fn page_size() -> usize {
        T::PAGE_SIZE
    }

    /// Returns the configured number of table levels.
    pub const fn table_levels() -> usize {
        T::LEVEL_BITS.len()
    }

    /// Returns the number of represented virtual-address bits.
    pub const fn valid_bits() -> usize {
        Frame::<T, A>::PT_VALID_BITS
    }

    fn is_addr_in_width(addr: usize) -> bool {
        let valid_bits = Self::valid_bits();
        if valid_bits >= usize::BITS as usize {
            return true;
        }
        addr < (1usize << valid_bits)
    }

    /// Releases the root and every intermediate table frame.
    ///
    /// # Safety
    ///
    /// No CPU or alias may access the table during or after this call. Mapped
    /// data frames are not released.
    pub unsafe fn destroy(mut self) {
        self.root.deallocate_recursive(Frame::<T, A>::PT_LEVEL);
    }

    /// Releases all table frames while leaving this view unusable.
    ///
    /// # Safety
    ///
    /// No CPU or alias may access the table during or after this call. Mapped
    /// data frames are not released.
    pub unsafe fn deallocate(&mut self) {
        self.root.deallocate_recursive(Frame::<T, A>::PT_LEVEL);
    }

    /// Translates a virtual address and returns its architecture entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the address is outside the configured width or no
    /// valid mapping exists.
    pub fn translate(&self, vaddr: VirtAddr) -> PagingResult<(PhysAddr, T::P)> {
        self.translate_with_level(vaddr)
            .map(|(phys_addr, pte, _)| (phys_addr, pte))
    }

    /// Translates a virtual address and returns the matched PTE level.
    pub fn translate_with_level(&self, vaddr: VirtAddr) -> PagingResult<(PhysAddr, T::P, usize)> {
        if T::STRICT_ADDRESS_WIDTH && !Self::is_addr_in_width(vaddr.as_usize()) {
            return Err(PagingError::address_overflow("translate"));
        }

        let (pte, level) = self
            .root
            .translate_recursive_with_level(vaddr, Frame::<T, A>::PT_LEVEL)?;

        let pte_config = pte.to_config(level > 1);

        let (phys_addr, _) = if pte_config.huge {
            let level_size = Frame::<T, A>::level_size(level);
            let offset_in_page = vaddr.as_usize() % level_size;
            (
                PhysAddr::from_usize(pte_config.paddr.as_usize() + offset_in_page),
                level_size,
            )
        } else {
            let offset_in_page = vaddr.as_usize() % T::PAGE_SIZE;
            (
                PhysAddr::from_usize(pte_config.paddr.as_usize() + offset_in_page),
                T::PAGE_SIZE,
            )
        };

        Ok((phys_addr, pte, level))
    }

    /// Translates a virtual address to a physical address.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::translate`].
    pub fn translate_phys(&self, vaddr: VirtAddr) -> PagingResult<PhysAddr> {
        let (p, _) = self.translate(vaddr)?;
        Ok(p)
    }

    /// Returns whether a virtual address has a valid mapping.
    pub fn is_mapped(&self, vaddr: VirtAddr) -> bool {
        self.translate(vaddr).is_ok()
    }

    /// Returns the physical address of the root table.
    pub fn root_paddr(&self) -> crate::flexible::PhysAddr {
        self.root.paddr
    }
}
