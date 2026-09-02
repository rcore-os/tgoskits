use crate::{
    FrameAllocator, PageTableEntry, PagingError, PagingResult, PhysAddr, PteConfigOf, TableMeta,
    VirtAddr, frame::Frame,
};

/// 页表映射配置
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MapConfig<C> {
    pub vaddr: VirtAddr,
    pub paddr: PhysAddr,
    pub size: usize,
    /// Page Table Entry 配置模板
    ///
    /// 所有页表项将使用此配置创建（除了物理地址位）
    pub pte: C,
    pub allow_huge: bool,
    pub flush: bool,
}

/// 内部映射递归配置
#[derive(Clone, Copy)]
pub struct MapRecursiveConfig<C> {
    pub start_vaddr: VirtAddr,
    pub start_paddr: PhysAddr,
    pub end_vaddr: VirtAddr,
    pub level: usize,
    pub allow_huge: bool,
    pub flush: bool,
    pub pte_template: C,
}

/// 取消映射配置
#[derive(Clone, Copy)]
pub struct UnmapConfig {
    pub start_vaddr: VirtAddr,
    pub size: usize,
    pub flush: bool,
}

/// 内部取消映射递归配置
#[derive(Clone, Copy)]
pub struct UnmapRecursiveConfig {
    pub start_vaddr: VirtAddr,
    pub end_vaddr: VirtAddr,
    pub level: usize,
    pub flush: bool,
    pub(crate) retained_root_entries: Option<(usize, usize)>,
}

impl<C> core::fmt::Debug for MapConfig<C> {
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
    A: FrameAllocator,
{
    /// 递归映射的核心实现
    pub fn map_range_recursive(
        &mut self,
        config: MapRecursiveConfig<PteConfigOf<T>>,
    ) -> PagingResult<()> {
        let mut vaddr = config.start_vaddr;
        let mut paddr = config.start_paddr;

        while vaddr < config.end_vaddr {
            let index = Self::virt_to_index(vaddr, config.level);
            let level_size = Self::level_size(config.level);
            let remaining_size = config.end_vaddr - vaddr;

            // 检查是否可以使用大页映射
            if config.allow_huge
                && config.level > 1
                && config.level <= T::MAX_BLOCK_LEVEL
                && level_size <= remaining_size
                && vaddr.as_usize().is_multiple_of(level_size)
                && paddr.as_usize().is_multiple_of(level_size)
            {
                // 创建大页映射
                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                if !pte_ref.unused() {
                    return Err(PagingError::mapping_conflict(vaddr, paddr));
                }
                *pte_ref = T::P::new_page(paddr, config.pte_template, true);

                // 如果需要刷新TLB，立即执行
                if config.flush {
                    T::flush(Some(vaddr));
                }

                vaddr = VirtAddr::from_usize(vaddr.as_usize().checked_add(level_size).ok_or_else(
                    || {
                        PagingError::address_overflow(
                            "Virtual address overflow in map_range_recursive",
                        )
                    },
                )?);
                paddr = PhysAddr::from_usize(paddr.as_usize().checked_add(level_size).ok_or_else(
                    || {
                        PagingError::address_overflow(
                            "Physical address overflow in map_range_recursive",
                        )
                    },
                )?);
                continue;
            }

            // 如果到达页表级别，进行普通页映射
            if config.level == 1 {
                // 创建普通页面映射
                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                if !pte_ref.unused() {
                    return Err(PagingError::mapping_conflict(vaddr, paddr));
                }

                *pte_ref = T::P::new_page(paddr, config.pte_template, false);

                // 如果需要刷新TLB，立即执行
                if config.flush {
                    T::flush(Some(vaddr));
                }

                vaddr = VirtAddr::from_usize(
                    vaddr.as_usize().checked_add(T::PAGE_SIZE).ok_or_else(|| {
                        PagingError::address_overflow(
                            "Virtual address overflow in map_range_recursive",
                        )
                    })?,
                );
                paddr = PhysAddr::from_usize(
                    paddr.as_usize().checked_add(T::PAGE_SIZE).ok_or_else(|| {
                        PagingError::address_overflow(
                            "Physical address overflow in map_range_recursive",
                        )
                    })?,
                );
                continue;
            }

            // 检查当前页表项状态并决定如何处理
            let allocator = self.allocator.clone();
            let current_pte = self.as_slice()[index];

            let child_frame = if !current_pte.unused() {
                if current_pte.huge(true) {
                    return Err(PagingError::mapping_conflict(
                        vaddr,
                        current_pte.paddr(true),
                    ));
                }
                if !current_pte.present() {
                    return Err(PagingError::hierarchy_error(
                        "Non-present intermediate entry is not a leaf",
                    ));
                }

                // 子页表已存在，获取它
                Frame::from_paddr(current_pte.paddr(true), allocator)
            } else {
                // 需要创建新的子页表
                let new_frame = Frame::<T, A>::new(allocator)?;
                let new_frame_paddr = new_frame.paddr;

                // 链接子页表 - 子页表指针必须是 NON_BLOCK（不是大页）
                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                *pte_ref = T::P::new_table(new_frame_paddr);

                new_frame
            };

            // 计算当前页表条目对应的范围结束地址。  All arithmetic is
            // checked: a wrapped boundary could make the recursive walker
            // revisit a low address and install aliases outside the request.
            let entry_base = (vaddr.as_usize() / level_size)
                .checked_mul(level_size)
                .ok_or_else(|| {
                    PagingError::address_overflow(
                        "Page-table entry base overflow in map_range_recursive",
                    )
                })?;
            // A sign-extended canonical address can legitimately occupy the
            // final page-table entry.  Its mathematical entry end is 2^N,
            // which is represented by the request's exclusive end only (and
            // cannot be stored in a `usize` address).  Clamp that one boundary
            // to the request end; every other overflow remains an error.
            let current_entry_end = match entry_base.checked_add(level_size) {
                Some(end) => end,
                None if config.end_vaddr.as_usize() > entry_base => config.end_vaddr.as_usize(),
                None => {
                    return Err(PagingError::address_overflow(
                        "Page-table entry end overflow in map_range_recursive",
                    ));
                }
            };
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

            // 计算本轮映射的虚拟地址范围
            let mapped_size = next_level_vaddr - vaddr;
            vaddr = next_level_vaddr;
            paddr = PhysAddr::from_usize(paddr.as_usize().checked_add(mapped_size).ok_or_else(
                || {
                    PagingError::address_overflow(
                        "Physical address overflow in map_range_recursive",
                    )
                },
            )?);
        }

        Ok(())
    }

    /// 递归取消映射的核心实现
    ///
    /// 返回值：bool 表示此帧是否为空（所有页表项都无效），可以回收
    pub fn unmap_range_recursive(&mut self, config: UnmapRecursiveConfig) -> PagingResult<bool> {
        let mut vaddr = config.start_vaddr;
        let mut can_reclaim = true;
        let allocator = self.allocator.clone();

        while vaddr < config.end_vaddr {
            let index = Self::virt_to_index(vaddr, config.level);
            let level_size = Self::level_size(config.level);
            let remaining_size = config.end_vaddr - vaddr;

            let entries = self.as_slice_mut();
            let pte_ref = &mut entries[index];

            // An invalid leaf can still retain its physical address. Treat it
            // as occupied state and clear it instead of skipping it.
            if pte_ref.unused() {
                vaddr = checked_advance(vaddr, level_size.min(remaining_size))?;
                continue;
            }

            if !pte_ref.present() {
                pte_ref.clear();
                if config.flush {
                    T::flush(Some(vaddr));
                }
                vaddr = checked_advance(vaddr, level_size.min(remaining_size))?;
                continue;
            }

            // 如果是叶子级别或者是大页，直接清除
            let is_huge = pte_ref.huge(config.level > 1);
            if config.level == 1 || is_huge {
                // 清除页表项
                pte_ref.clear();

                // 刷新TLB
                if config.flush {
                    T::flush(Some(vaddr));
                }

                vaddr = checked_advance(vaddr, if is_huge { level_size } else { T::PAGE_SIZE })?;
                continue;
            }

            // 中间级别：递归处理子页表
            // 需要在修改pte_ref之前获取所需信息
            let child_paddr = pte_ref.paddr(true);

            // 计算当前页表条目对应的范围结束地址
            let entry_base = (vaddr.as_usize() / level_size)
                .checked_mul(level_size)
                .ok_or_else(|| {
                    PagingError::address_overflow(
                        "Page-table entry base overflow in unmap_range_recursive",
                    )
                })?;
            let current_entry_end = match entry_base.checked_add(level_size) {
                Some(end) => end,
                None if config.end_vaddr.as_usize() > entry_base => config.end_vaddr.as_usize(),
                None => {
                    return Err(PagingError::address_overflow(
                        "Page-table entry end overflow in unmap_range_recursive",
                    ));
                }
            };
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
                    retained_root_entries: None,
                };

                // 递归取消子页表映射
                let child_can_reclaim = child_frame.unmap_range_recursive(child_config)?;

                if child_can_reclaim
                    && config
                        .retained_root_entries
                        .is_some_and(|(start, end)| start <= index && index < end)
                {
                    can_reclaim = false;
                } else if child_can_reclaim {
                    // 子页表完全为空，可以回收
                    // 清除指向子页表的PTE
                    pte_ref.clear();
                    allocator.dealloc_frame(child_paddr);
                } else {
                    // 子页表仍有有效映射，不能回收
                    can_reclaim = false;
                }
            }

            vaddr = next_level_vaddr;
        }

        // 检查此帧是否完全为空
        if can_reclaim {
            can_reclaim = self.is_frame_empty();
        }

        Ok(can_reclaim)
    }

    /// 检查页表帧是否全为空（所有页表项都未使用）
    fn is_frame_empty(&self) -> bool {
        let entries = self.as_slice();
        for pte in entries {
            if !pte.unused() {
                return false;
            }
        }
        true
    }
}

fn checked_advance(address: VirtAddr, amount: usize) -> PagingResult<VirtAddr> {
    address
        .as_usize()
        .checked_add(amount)
        .map(VirtAddr::from_usize)
        .ok_or_else(|| PagingError::address_overflow("page-table address advance overflow"))
}
