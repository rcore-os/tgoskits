//! Recursive page-table mapping and unmapping operations.

use super::{
    PageFrameProvider, PageTableEntry, PagingError, PagingResult, PhysAddr, PteConfig, TableMeta,
    VirtAddr, frame::Frame,
};

/// Configuration for mapping one contiguous virtual and physical range.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MapConfig {
    pub vaddr: VirtAddr,
    pub paddr: PhysAddr,
    pub size: usize,
    /// PTE template; the mapper supplies the physical address bits.
    pub pte: PteConfig,
    pub allow_huge: bool,
    pub flush: bool,
}

/// State carried while descending through a mapping operation.
#[derive(Clone, Copy)]
pub(crate) struct MapRecursiveConfig {
    pub start_vaddr: VirtAddr,
    pub start_paddr: PhysAddr,
    pub end_vaddr: VirtAddr,
    pub level: usize,
    pub allow_huge: bool,
    pub flush: bool,
    pub pte_template: PteConfig,
}

/// Configuration for unmapping one virtual range.
#[derive(Clone, Copy)]
pub struct UnmapConfig {
    pub start_vaddr: VirtAddr,
    pub size: usize,
    pub flush: bool,
}

/// State carried while descending through an unmap operation.
#[derive(Clone, Copy)]
pub(crate) struct UnmapRecursiveConfig {
    pub start_vaddr: VirtAddr,
    pub end_vaddr: VirtAddr,
    pub level: usize,
    pub flush: bool,
}

impl core::fmt::Debug for MapConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MapConfig")
            .field("vaddr", &format_args!("{:#x}", self.vaddr.as_usize()))
            .field("paddr", &format_args!("{:#x}", self.paddr.as_usize()))
            .field("size", &format_args!("{:#x}", self.size))
            .field("allow_huge", &self.allow_huge)
            .field("flush", &self.flush)
            .finish()
    }
}

impl<T, A> Frame<T, A>
where
    T: TableMeta,
    A: PageFrameProvider,
{
    /// Maps a range while descending only through the levels it intersects.
    pub(crate) fn map_range_recursive(&mut self, config: MapRecursiveConfig) -> PagingResult<()> {
        let mut vaddr = config.start_vaddr;
        let mut paddr = config.start_paddr;

        while vaddr < config.end_vaddr {
            let index = Self::virt_to_index(vaddr, config.level);
            let level_size = Self::level_size(config.level);
            let remaining_size = config.end_vaddr - vaddr;

            if config.allow_huge
                && config.level > 1
                && config.level <= T::MAX_BLOCK_LEVEL
                && level_size <= remaining_size
                && vaddr.as_usize().is_multiple_of(level_size)
                && paddr.as_usize().is_multiple_of(level_size)
            {
                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                if pte_ref.valid() {
                    return Err(PagingError::AlreadyMapped);
                }
                let mut pte_config = config.pte_template;
                pte_config.paddr = paddr;
                pte_config.valid = true;
                pte_config.huge = true;
                pte_config.is_dir = true;

                *pte_ref = T::P::from_config(pte_config);

                if config.flush {
                    T::flush(Some(vaddr));
                }

                vaddr += level_size;
                paddr += level_size;
                continue;
            }

            if config.level == 1 {
                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                if pte_ref.valid() {
                    return Err(PagingError::AlreadyMapped);
                }

                let mut pte_config = config.pte_template;
                pte_config.paddr = paddr;
                pte_config.valid = true;
                pte_config.huge = false;
                pte_config.is_dir = false;

                *pte_ref = T::P::from_config(pte_config);

                if config.flush {
                    T::flush(Some(vaddr));
                }

                vaddr += T::PAGE_SIZE;
                paddr += T::PAGE_SIZE;
                continue;
            }

            let allocator = self.allocator.clone();
            let current_pte = self.as_slice()[index];
            let current_config = current_pte.to_config(true);

            let child_frame = if current_config.valid {
                if current_config.huge {
                    return Err(PagingError::HierarchyError {
                        details: "Cannot create page table under huge page",
                    });
                }

                Frame::from_paddr(current_config.paddr, allocator)
            } else {
                let new_frame = Frame::<T, A>::new(allocator)?;
                let new_frame_paddr = new_frame.paddr;

                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                let pte_config = PteConfig {
                    paddr: new_frame_paddr,
                    valid: true,
                    huge: false,
                    is_dir: true,
                    ..config.pte_template
                };
                *pte_ref = T::P::from_config(pte_config);

                new_frame
            };

            // Saturation keeps a final top-of-address-space range bounded.
            let current_entry_end = (vaddr.as_usize() / level_size)
                .saturating_add(1)
                .saturating_mul(level_size);
            let next_level_vaddr =
                VirtAddr::from_usize(current_entry_end.min(config.end_vaddr.as_usize()));
            let mut child_frame = child_frame;
            let child_config = MapRecursiveConfig {
                start_vaddr: vaddr,
                start_paddr: paddr,
                end_vaddr: next_level_vaddr,
                level: config.level - 1,
                allow_huge: config.allow_huge,
                flush: config.flush,
                pte_template: config.pte_template,
            };
            child_frame.map_range_recursive(child_config)?;

            let mapped_size = next_level_vaddr - vaddr;
            vaddr = next_level_vaddr;
            paddr += mapped_size;
        }

        Ok(())
    }

    /// Unmaps a range and reports whether the current table became empty.
    pub(crate) fn unmap_range_recursive(
        &mut self,
        config: UnmapRecursiveConfig,
    ) -> PagingResult<bool> {
        let mut vaddr = config.start_vaddr;
        let mut can_reclaim = true;
        let allocator = self.allocator.clone();

        while vaddr < config.end_vaddr {
            let index = Self::virt_to_index(vaddr, config.level);
            let level_size = Self::level_size(config.level);
            let remaining_size = config.end_vaddr - vaddr;

            let entries = self.as_slice_mut();
            let pte_ref = &mut entries[index];

            let pte_config = pte_ref.to_config(config.level > 1);
            if !pte_config.valid {
                vaddr += level_size.min(remaining_size);
                continue;
            }

            if config.level == 1 || pte_config.huge {
                let invalid_config = PteConfig {
                    valid: false,
                    ..Default::default()
                };
                *pte_ref = T::P::from_config(invalid_config);

                if config.flush {
                    T::flush(Some(vaddr));
                }

                vaddr += if pte_config.huge {
                    level_size
                } else {
                    T::PAGE_SIZE
                };
                continue;
            }

            let child_paddr = pte_config.paddr;

            let current_entry_end = ((vaddr.as_usize() / level_size) + 1) * level_size;
            let next_level_vaddr =
                VirtAddr::from_usize(current_entry_end.min(config.end_vaddr.as_usize()));

            {
                let mut child_frame: Frame<T, A> =
                    Frame::from_paddr(child_paddr, allocator.clone());
                let child_config = UnmapRecursiveConfig {
                    start_vaddr: vaddr,
                    end_vaddr: next_level_vaddr,
                    level: config.level - 1,
                    flush: config.flush,
                };

                let child_can_reclaim = child_frame.unmap_range_recursive(child_config)?;

                if child_can_reclaim {
                    let invalid_config = PteConfig {
                        valid: false,
                        ..Default::default()
                    };
                    *pte_ref = T::P::from_config(invalid_config);
                    allocator.dealloc_frame(child_paddr);
                } else {
                    can_reclaim = false;
                }
            }

            vaddr = next_level_vaddr;
        }

        if can_reclaim {
            can_reclaim = self
                .as_slice()
                .iter()
                .all(|pte| !pte.to_config(false).valid);
        }

        Ok(can_reclaim)
    }
}
