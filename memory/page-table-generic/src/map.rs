use crate::{
    FrameAllocator, PageTableEntry, PagingError, PagingResult, PhysAddr, PteConfig, TableMeta,
    VirtAddr, frame::Frame,
};

/// 页表映射配置
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MapConfig {
    pub vaddr: VirtAddr,
    pub paddr: PhysAddr,
    pub size: usize,
    /// Page Table Entry 配置模板
    ///
    /// 所有页表项将使用此配置创建（除了物理地址位）
    pub pte: PteConfig,
    pub allow_huge: bool,
    pub flush: bool,
}

/// 内部映射递归配置
#[derive(Clone, Copy)]
pub struct MapRecursiveConfig {
    pub start_vaddr: VirtAddr,
    pub start_paddr: PhysAddr,
    pub end_vaddr: VirtAddr,
    pub level: usize,
    pub allow_huge: bool,
    pub flush: bool,
    pub pte_template: PteConfig,
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
}

impl core::fmt::Debug for MapConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MapConfig")
            .field("vaddr", &format_args!("{:#x}", self.vaddr.raw()))
            .field("paddr", &format_args!("{:#x}", self.paddr.raw()))
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
    pub fn map_range_recursive(&mut self, config: MapRecursiveConfig) -> PagingResult<()> {
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
                && vaddr.raw().is_multiple_of(level_size)
                && paddr.raw().is_multiple_of(level_size)
            {
                // 创建大页映射
                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                if pte_ref.valid() {
                    return Err(PagingError::mapping_conflict(vaddr, paddr));
                }
                let mut pte_config = config.pte_template;
                pte_config.paddr = paddr;
                pte_config.valid = true;
                pte_config.huge = true;
                pte_config.is_dir = true;

                *pte_ref = T::P::from_config(pte_config);

                // 如果需要刷新TLB，立即执行
                if config.flush {
                    T::flush(Some(vaddr));
                }

                vaddr += level_size;
                paddr += level_size;
                continue;
            }

            // 如果到达页表级别，进行普通页映射
            if config.level == 1 {
                // 创建普通页面映射
                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                if pte_ref.valid() {
                    return Err(PagingError::mapping_conflict(vaddr, paddr));
                }

                let mut pte_config = config.pte_template;
                pte_config.paddr = paddr;
                pte_config.valid = true;
                pte_config.huge = false;
                pte_config.is_dir = false;

                *pte_ref = T::P::from_config(pte_config);

                // 如果需要刷新TLB，立即执行
                if config.flush {
                    T::flush(Some(vaddr));
                }

                vaddr += T::PAGE_SIZE;
                paddr += T::PAGE_SIZE;
                continue;
            }

            // 检查当前页表项状态并决定如何处理
            let allocator = self.allocator.clone();
            let current_pte = self.as_slice()[index];
            let current_config = current_pte.to_config(true);

            let child_frame = if current_config.valid {
                // 目录项（config.level > 1）可能有大页
                if current_config.huge {
                    return Err(PagingError::hierarchy_error(
                        "Cannot create page table under huge page",
                    ));
                }

                // 子页表已存在，获取它
                Frame::from_paddr(current_config.paddr, allocator)
            } else {
                // 需要创建新的子页表
                let new_frame = Frame::<T, A>::new(allocator)?;
                let new_frame_paddr = new_frame.paddr;

                // 链接子页表 - 子页表指针必须是 NON_BLOCK（不是大页）
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

            // 计算当前页表条目对应的范围结束地址
            // 使用 saturating 操作防止溢出，同时确保不超过地址空间最大值
            let current_entry_end = (vaddr.raw() / level_size)
                .saturating_add(1)
                .saturating_mul(level_size);
            let next_level_vaddr = VirtAddr::new(current_entry_end.min(config.end_vaddr.raw()));
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
            paddr += mapped_size;
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

            // 检查当前页表项是否有效
            let pte_config = pte_ref.to_config(config.level > 1);
            if !pte_config.valid {
                // 页表项无效，直接跳过
                // 注意：无效项不影响can_reclaim，因为我们只关心是否还有有效项
                vaddr += level_size.min(remaining_size);
                continue;
            }

            // 如果是叶子级别或者是大页，直接清除
            if config.level == 1 || pte_config.huge {
                // 清除页表项
                let invalid_config = PteConfig {
                    valid: false,
                    ..Default::default()
                };
                *pte_ref = T::P::from_config(invalid_config);

                // 刷新TLB
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

            // 中间级别：递归处理子页表
            // 需要在修改pte_ref之前获取所需信息
            let child_paddr = pte_config.paddr;

            // 计算当前页表条目对应的范围结束地址
            let current_entry_end = ((vaddr.raw() / level_size) + 1) * level_size;
            let next_level_vaddr = VirtAddr::new(current_entry_end.min(config.end_vaddr.raw()));

            {
                let mut child_frame: Frame<T, A> =
                    Frame::from_paddr(child_paddr, allocator.clone());
                let child_config = UnmapRecursiveConfig {
                    start_vaddr: vaddr,
                    end_vaddr: next_level_vaddr,
                    level: config.level - 1,
                    flush: config.flush,
                };

                // 递归取消子页表映射
                let child_can_reclaim = child_frame.unmap_range_recursive(child_config)?;

                if child_can_reclaim {
                    // 子页表完全为空，可以回收
                    // 清除指向子页表的PTE
                    let invalid_config = PteConfig {
                        valid: false,
                        ..Default::default()
                    };
                    *pte_ref = T::P::from_config(invalid_config);
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

    /// 检查页表帧是否全为空（所有页表项都无效）
    fn is_frame_empty(&self) -> bool {
        let entries = self.as_slice();
        for pte in entries {
            if pte.to_config(false).valid {
                return false;
            }
        }
        true
    }
}
