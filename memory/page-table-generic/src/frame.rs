use crate::{
    FrameAllocator, PageTableEntry, PagingError, PagingResult, PhysAddr, PteConfig, TableMeta,
    VirtAddr,
};

/// 页表帧，代表一个物理页面上的页表
#[derive(Clone, Copy)]
pub struct Frame<T: TableMeta, A: FrameAllocator> {
    pub paddr: PhysAddr,
    pub allocator: A,
    _marker: core::marker::PhantomData<T>,
}

impl<T: TableMeta, A: FrameAllocator> core::fmt::Debug for Frame<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Frame")
            .field("paddr", &format_args!("{:#x}", self.paddr.raw()))
            .finish()
    }
}

impl<T, A> Frame<T, A>
where
    T: TableMeta,
    A: FrameAllocator,
{
    pub(crate) const PT_INDEX_SHIFT: usize = T::PAGE_SIZE.trailing_zeros() as usize;
    pub(crate) const PT_INDEX_BITS: usize = cal_index_bits::<T>();
    pub(crate) const PT_VALID_BITS: usize = Self::PT_INDEX_BITS + Self::PT_INDEX_SHIFT;
    pub(crate) const LEN: usize = T::PAGE_SIZE / core::mem::size_of::<T::P>();
    pub(crate) const PT_LEVEL: usize = T::LEVEL_BITS.len();

    /// 创建新的页表帧（分配并清零）
    pub fn new(allocator: A) -> PagingResult<Self> {
        let paddr = allocator.alloc_frame().ok_or(PagingError::NoMemory)?;
        unsafe {
            let vaddr = allocator.phys_to_virt(paddr);
            core::ptr::write_bytes(vaddr, 0, T::PAGE_SIZE);
        }

        Ok(Self {
            paddr,
            allocator,
            _marker: core::marker::PhantomData,
        })
    }

    /// 从物理地址创建Frame（不分配）
    pub fn from_paddr(paddr: PhysAddr, allocator: A) -> Self {
        Self {
            paddr,
            allocator,
            _marker: core::marker::PhantomData,
        }
    }

    /// 从PTE创建子Frame（用于遍历子页表）
    pub fn from_pte(pte: &T::P, level: usize, allocator: A) -> Self {
        let config = pte.to_config(level > 1);
        Self::from_paddr(config.paddr, allocator)
    }

    /// 获取页表项的可变切片
    pub fn as_slice_mut(&mut self) -> &mut [T::P] {
        let vaddr = self.allocator.phys_to_virt(self.paddr);
        unsafe { core::slice::from_raw_parts_mut(vaddr as *mut T::P, Self::LEN) }
    }

    /// 获取页表项的不可变切片
    pub fn as_slice(&self) -> &[T::P] {
        let vaddr = self.allocator.phys_to_virt(self.paddr);
        unsafe { core::slice::from_raw_parts(vaddr as *const T::P, Self::LEN) }
    }

    /// 计算指定级别对应的映射大小
    /// - Level 1 (叶子): PAGE_SIZE
    /// - Level 2: PAGE_SIZE << LEVEL_BITS[最后一级]
    /// - Level 3: PAGE_SIZE << (LEVEL_BITS[最后一级] + LEVEL_BITS[倒数第二级])
    /// - Level N: PAGE_SIZE << (sum of LEVEL_BITS from last to N-1)
    pub fn level_size(level: usize) -> usize {
        if level == 1 {
            return T::PAGE_SIZE;
        }
        // 从最后一级开始累加位数，直到当前级别的前一级
        // 例如：对于 4 级页表 [9,9,9,9]，level=3 时，累加 LEVEL_BITS[3] (即最后一级 9 位)
        let total_levels = T::LEVEL_BITS.len();
        let shift = T::LEVEL_BITS
            .iter()
            .skip(total_levels - level + 1)
            .sum::<usize>();
        T::PAGE_SIZE << shift
    }

    /// 计算指定级别的页表索引
    /// 从虚拟地址中提取对应级别的索引位
    pub fn virt_to_index(vaddr: VirtAddr, level: usize) -> usize {
        if level == 0 || level > Self::PT_LEVEL {
            panic!("Invalid level: {} (valid: 1..={})", level, Self::PT_LEVEL);
        }

        // 计算需要跳过的位数（页面偏移 + 低级别索引位）
        // Level 1 (叶子): shift = page_shift（只跳过页面偏移）
        // Level 2: shift = page_shift + LEVEL_BITS[最后一级]
        // Level 3: shift = page_shift + LEVEL_BITS[最后一级] + LEVEL_BITS[倒数第二级]
        // Level N: shift = page_shift + sum(LEVEL_BITS[N+1..end])
        let page_shift = T::PAGE_SIZE.trailing_zeros() as usize;
        let total_levels = T::LEVEL_BITS.len();

        // 累加从最后一级到当前级别之后的所有位数
        let shift = if level == 1 {
            page_shift
        } else {
            page_shift
                + T::LEVEL_BITS
                    .iter()
                    .skip(total_levels - level + 1)
                    .sum::<usize>()
        };

        // 当前级别的索引位数
        let level_index_bits = T::LEVEL_BITS[total_levels - level];
        let mask = (1 << level_index_bits) - 1;

        (vaddr.raw() >> shift) & mask
    }

    /// 重建完整的虚拟地址
    /// 从基地址和索引计算完整的虚拟地址
    pub fn reconstruct_vaddr(index: usize, level: usize, base_vaddr: VirtAddr) -> VirtAddr {
        let entry_size = Self::level_size(level);
        base_vaddr + index * entry_size
    }

    /// 递归释放当前帧及所有子帧
    ///
    /// 此方法会：
    /// 1. 递归释放所有有效的子页表帧
    /// 2. 清除所有页表项（设为invalid）
    /// 3. 释放当前帧
    ///
    /// 注意：只释放页表帧，不释放映射的物理页（数据页/大页）
    ///
    /// # Parameters
    /// - `level`: 当前帧所在的页表级别（1=叶子，数字越大级别越高）
    ///
    /// # Safety
    /// 调用者必须确保：
    /// - 没有其他代码在访问这些页表
    /// - 没有CPU正在使用这些页表进行地址翻译
    pub fn deallocate_recursive(&mut self, level: usize) {
        // 先递归释放所有子帧
        self.deallocate_children(level);

        // 再释放当前帧
        self.allocator.dealloc_frame(self.paddr);
    }

    /// 只释放子页表帧，保留当前帧
    ///
    /// 遍历当前帧中的所有页表项：
    /// - 如果是大页或叶子级别的数据页：跳过（不释放物理页，也不清除映射）
    /// - 如果是非叶子级别的页表指针：递归释放子页表帧，并清除PTE
    ///
    /// # Parameters
    /// - `level`: 当前帧所在的页表级别（1=叶子，数字越大级别越高）
    pub fn deallocate_children(&mut self, level: usize) {
        // 反向遍历以避免索引变化问题
        for i in (0..Self::LEN).rev() {
            // 先获取当前PTE的状态
            let entry_info = {
                let entries = self.as_slice();
                if i < entries.len() {
                    let config = entries[i].to_config(level > 1);
                    (config.valid, config.huge, config.paddr)
                } else {
                    (false, false, crate::PhysAddr::new(0))
                }
            };

            let (is_valid, is_huge, paddr) = entry_info;

            if !is_valid {
                continue;
            }

            // 如果是大页或叶子级别的数据页：跳过，保持映射不变
            if is_huge || level == 1 {
                continue;
            }
            // 否则是非叶子级别的页表指针，递归释放子页表帧
            else {
                let mut child_frame = Frame::<T, A>::from_paddr(paddr, self.allocator.clone());
                child_frame.deallocate_recursive(level - 1);

                // 子页表帧已释放，清除PTE
                let entries_mut = self.as_slice_mut();
                let invalid_config = PteConfig {
                    valid: false,
                    ..Default::default()
                };
                entries_mut[i] = T::P::from_config(invalid_config);
            }
        }
    }

    /// 递归查找虚拟地址对应的页表项
    ///
    /// # 参数
    /// - `vaddr`: 要查找的虚拟地址
    /// - `level`: 当前页表级别
    ///
    /// # 返回值
    /// - `Ok(T::P)`: 找到的页表项
    /// - `Err(PagingError)`: 查找失败
    pub fn translate_recursive(&self, vaddr: VirtAddr, level: usize) -> PagingResult<T::P> {
        let (pte, _) = self.translate_recursive_with_level(vaddr, level)?;
        Ok(pte)
    }

    /// 递归查找虚拟地址对应的页表项，同时返回该PTE所在的级别
    ///
    /// # 参数
    /// - `vaddr`: 要查找的虚拟地址
    /// - `level`: 当前页表级别
    ///
    /// # 返回值
    /// - `Ok((T::P, usize))`: 找到的页表项及其所在的级别
    /// - `Err(PagingError)`: 查找失败
    pub fn translate_recursive_with_level(
        &self,
        vaddr: VirtAddr,
        level: usize,
    ) -> PagingResult<(T::P, usize)> {
        // 计算当前级别的页表索引
        let index = Self::virt_to_index(vaddr, level);

        // 获取页表项
        let entries = self.as_slice();
        let pte = entries[index];

        // 检查页表项是否有效
        let config = pte.to_config(level > 1);
        if !config.valid {
            return Err(PagingError::not_mapped());
        }

        // 如果是大页映射或叶子级别，直接返回页表项及其级别
        if config.huge || level == 1 {
            return Ok((pte, level));
        }

        // 否则，继续递归到下一级页表
        if level > 1 {
            let child_frame: Frame<T, A> = Frame::from_pte(&pte, level, self.allocator.clone());
            return child_frame.translate_recursive_with_level(vaddr, level - 1);
        }

        // 不应该到达这里
        Err(PagingError::hierarchy_error(
            "Invalid page table level during translation",
        ))
    }

    /// 递归释放指定的单个页表项
    ///
    /// 如果该PTE指向有效的子页表，则递归释放该子页表及其所有子帧
    /// 在释放前将PTE设为invalid
    ///
    /// 注意：只释放页表帧，不释放映射的物理页
    ///
    /// # Parameters
    /// - `index`: 要释放的PTE索引
    /// - `level`: 当前帧所在的页表级别
    pub fn dealloc_entry_recursive(&mut self, index: usize, level: usize) -> bool {
        if index >= Self::LEN || level <= 1 {
            return false;
        }

        let entries = self.as_slice();
        let entry = &entries[index];
        let config = entry.to_config(level > 1);

        if config.valid && !config.huge {
            // 递归释放子帧（子帧的级别是 level - 1）
            let mut child_frame = Frame::<T, A>::from_pte(entry, level, self.allocator.clone());
            child_frame.deallocate_recursive(level - 1);

            // 将当前PTE设为invalid
            let entries_mut = self.as_slice_mut();
            let invalid_config = PteConfig {
                valid: false,
                ..Default::default()
            };
            entries_mut[index] = T::P::from_config(invalid_config);

            true
        } else {
            false
        }
    }
}

const fn cal_index_bits<T: TableMeta>() -> usize {
    let mut bits = 0;
    let len = T::LEVEL_BITS.len();
    let mut i = 0;
    while i < len {
        bits += T::LEVEL_BITS[i];
        i += 1;
    }
    bits
}
