use crate::{
    FrameAllocator, PageTableEntry, PagingError, PagingResult, PhysAddr, TableGeneric, VirtAddr,
    frame::Frame,
};

/// 页表映射配置
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MapConfig<P: PageTableEntry> {
    pub vaddr: VirtAddr,
    pub paddr: PhysAddr,
    pub size: usize,
    /// Page Table Entry
    ///
    /// All pte will be set as this value, except the address bits
    pub pte: P,
    pub allow_huge: bool,
    pub flush: bool,
}

/// 内部映射递归配置
#[derive(Clone, Copy)]
pub struct MapRecursiveConfig<P: PageTableEntry> {
    pub start_vaddr: VirtAddr,
    pub start_paddr: PhysAddr,
    pub end_vaddr: VirtAddr,
    pub level: usize,
    pub allow_huge: bool,
    pub flush: bool,
    pub pte_template: P,
}

impl<P: PageTableEntry> core::fmt::Debug for MapConfig<P> {
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
    T: TableGeneric,
    A: FrameAllocator,
{
    /// 递归映射的核心实现
    pub fn map_range_recursive(&mut self, config: MapRecursiveConfig<T::P>) -> PagingResult<()> {
        let mut vaddr = config.start_vaddr;
        let mut paddr = config.start_paddr;

        while vaddr < config.end_vaddr {
            let index = Self::virt_to_index(vaddr, config.level);
            let level_size = Self::level_size(config.level);
            let remaining_size = config.end_vaddr - vaddr;

            // 检查是否可以使用大页映射
            if config.allow_huge
                && config.level <= T::MAX_BLOCK_LEVEL
                && level_size <= remaining_size
                && vaddr.raw() % level_size == 0
                && paddr.raw() % level_size == 0
            {
                // 创建大页映射
                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                if pte_ref.valid() {
                    return Err(PagingError::mapping_conflict(vaddr, pte_ref.paddr()));
                }

                let mut new_pte = config.pte_template;
                new_pte.set_paddr(paddr);
                new_pte.set_valid(true);
                new_pte.set_is_huge(true);
                *pte_ref = new_pte;

                vaddr += level_size;
                paddr += level_size;
                continue;
            }

            // 如果到达页表级别，进行普通页映射
            if config.level == 1 {
                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                if pte_ref.valid() {
                    return Err(PagingError::mapping_conflict(vaddr, pte_ref.paddr()));
                }

                let mut new_pte = config.pte_template;
                new_pte.set_paddr(paddr);
                new_pte.set_valid(true);
                new_pte.set_is_huge(false);
                *pte_ref = new_pte;

                vaddr += T::PAGE_SIZE;
                paddr += T::PAGE_SIZE;
                continue;
            }

            // 检查当前页表项状态并决定如何处理
            let allocator = self.allocator.clone();
            let current_pte = self.as_slice()[index];

            let child_frame = if current_pte.valid() {
                if current_pte.is_huge() {
                    return Err(PagingError::hierarchy_error(
                        "Cannot create page table under huge page",
                    ));
                }

                // 子页表已存在，获取它
                Frame::from_paddr(current_pte.paddr(), allocator)
            } else {
                // 需要创建新的子页表
                let new_frame = Frame::<T, A>::new(allocator)?;
                let new_frame_paddr = new_frame.paddr;

                // 链接子页表
                let entries = self.as_slice_mut();
                let pte_ref = &mut entries[index];
                let mut new_pte = config.pte_template;
                new_pte.set_paddr(new_frame_paddr);
                new_pte.set_valid(true);
                new_pte.set_is_huge(false);
                *pte_ref = new_pte;

                new_frame
            };

            let next_level_vaddr = vaddr + level_size.min(config.end_vaddr - vaddr);
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
}
