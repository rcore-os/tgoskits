use crate::{
    FrameAllocator, PageTableEntry, PagingError, PagingResult, PhysAddr, PteConfigOf, TableMeta,
    VirtAddr,
};

/// How a huge-leaf split initializes the reserved child table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HugeSplitFill {
    /// Preserve the mapping by materializing finer leaves with the same frame
    /// and PTE configuration.
    Inherit,
    /// Install an empty child table for a caller that will populate individual
    /// leaves while holding its page-table mutation domain.
    Empty,
}

/// A page-table frame detached from a live page-table tree.
///
/// Detaching only clears the parent entry and transfers the allocator
/// capability into this value; it deliberately does not release the physical
/// memory.  A caller can therefore couple reclamation to a TLB acknowledgement
/// (or another quiescence protocol) instead of freeing a frame while a CPU may
/// still walk the old root.  The token is consumed by [`Self::reclaim`], so a
/// frame is released at most once.
pub struct DetachedPageTableFrame<A: FrameAllocator> {
    paddr: PhysAddr,
    frames: usize,
    frame_size: usize,
    allocator: A,
}

impl<A: FrameAllocator> core::fmt::Debug for DetachedPageTableFrame<A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DetachedPageTableFrame")
            .field("paddr", &format_args!("{:#x}", self.paddr.as_usize()))
            .field("frames", &self.frames)
            .field("frame_size", &self.frame_size)
            .finish()
    }
}

impl<A: FrameAllocator> DetachedPageTableFrame<A> {
    pub(crate) fn new(paddr: PhysAddr, frames: usize, frame_size: usize, allocator: A) -> Self {
        Self {
            paddr,
            frames,
            frame_size,
            allocator,
        }
    }

    /// Releases the detached frame range after the caller has established
    /// that no hardware translation can still reference it.
    pub fn reclaim(self) {
        self.allocator
            .dealloc_frames(self.paddr, self.frames, self.frame_size);
    }
}

/// 页表帧，代表一个物理页面上的页表
#[derive(Clone, Copy)]
pub struct Frame<T: TableMeta, A: FrameAllocator> {
    pub(crate) paddr: PhysAddr,
    pub(crate) allocator: A,
    frames: usize,
    _marker: core::marker::PhantomData<T>,
}

impl<T: TableMeta, A: FrameAllocator> core::fmt::Debug for Frame<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Frame")
            .field("paddr", &format_args!("{:#x}", self.paddr.as_usize()))
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
    pub(crate) const ROOT_LEN: usize = 1usize << T::LEVEL_BITS[0];
    pub(crate) const ROOT_FRAMES: usize =
        (Self::ROOT_LEN * core::mem::size_of::<T::P>()).div_ceil(T::PAGE_SIZE);
    pub(crate) const PT_LEVEL: usize = T::LEVEL_BITS.len();

    /// 创建新的页表帧（分配并清零）
    pub(crate) fn new(allocator: A) -> PagingResult<Self> {
        let paddr = allocator.alloc_frame().ok_or(PagingError::NoMemory)?;
        unsafe {
            let vaddr = allocator.phys_to_virt(paddr);
            core::ptr::write_bytes(vaddr, 0, T::PAGE_SIZE);
        }

        Ok(Self {
            paddr,
            allocator,
            frames: 1,
            _marker: core::marker::PhantomData,
        })
    }

    /// 创建新的根页表帧。
    pub(crate) fn new_root(allocator: A) -> PagingResult<Self> {
        let align = T::PAGE_SIZE * Self::ROOT_FRAMES;
        let paddr = allocator
            .alloc_frames(Self::ROOT_FRAMES, align)
            .ok_or(PagingError::NoMemory)?;
        unsafe {
            let vaddr = allocator.phys_to_virt(paddr);
            core::ptr::write_bytes(vaddr, 0, T::PAGE_SIZE * Self::ROOT_FRAMES);
        }

        Ok(Self {
            paddr,
            allocator,
            frames: Self::ROOT_FRAMES,
            _marker: core::marker::PhantomData,
        })
    }

    /// 从物理地址创建Frame（不分配）
    pub(crate) fn from_paddr(paddr: PhysAddr, allocator: A) -> Self {
        Self {
            paddr,
            allocator,
            frames: 1,
            _marker: core::marker::PhantomData,
        }
    }

    /// 从根页表物理地址创建Frame（不分配）
    pub(crate) fn from_root_paddr(paddr: PhysAddr, allocator: A) -> Self {
        Self {
            paddr,
            allocator,
            frames: Self::ROOT_FRAMES,
            _marker: core::marker::PhantomData,
        }
    }

    /// 从PTE创建子Frame（用于遍历子页表）
    pub(crate) fn from_pte(pte: &T::P, level: usize, allocator: A) -> Self {
        Self::from_paddr(pte.paddr(level > 1), allocator)
    }

    /// 获取页表项的可变切片
    pub(crate) fn as_slice_mut(&mut self) -> &mut [T::P] {
        let vaddr = self.allocator.phys_to_virt(self.paddr);
        unsafe { core::slice::from_raw_parts_mut(vaddr as *mut T::P, self.len()) }
    }

    /// 获取页表项的不可变切片
    pub(crate) fn as_slice(&self) -> &[T::P] {
        let vaddr = self.allocator.phys_to_virt(self.paddr);
        unsafe { core::slice::from_raw_parts(vaddr as *const T::P, self.len()) }
    }

    pub fn len(&self) -> usize {
        self.frames * Self::LEN
    }

    /// Returns whether this frame range contains no page-table entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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

        (vaddr.as_usize() >> shift) & mask
    }

    pub(crate) fn level_for_page_size(page_size: usize) -> Option<usize> {
        (1..=Self::PT_LEVEL).find(|level| Self::level_size(*level) == page_size)
    }

    pub fn protect_recursive(
        &mut self,
        vaddr: VirtAddr,
        config: PteConfigOf<T>,
        level: usize,
    ) -> PagingResult<usize> {
        let index = Self::virt_to_index(vaddr, level);
        let entry = self.as_slice()[index];
        if entry.unused() {
            return Err(PagingError::not_mapped());
        }
        let is_dir = level > 1;
        let is_huge = entry.huge(is_dir);
        if is_huge || level == 1 {
            self.as_slice_mut()[index] = T::P::new_page(entry.paddr(is_dir), config, is_huge);
            return Ok(Self::level_size(level));
        }
        if !entry.present() {
            return Err(PagingError::not_mapped());
        }

        let mut child = Self::from_paddr(entry.paddr(is_dir), self.allocator.clone());
        child.protect_recursive(vaddr, config, level - 1)
    }

    /// Splits the block mapping that contains `vaddr` until `boundary` is also
    /// a mapping boundary. Existing leaf attributes and physical addresses are
    /// preserved in every child entry.
    pub(crate) fn split_leaf_for_boundary(
        &mut self,
        vaddr: VirtAddr,
        boundary: VirtAddr,
        level: usize,
    ) -> PagingResult {
        let index = Self::virt_to_index(vaddr, level);
        let entry = self.as_slice()[index];
        if entry.unused() || level == 1 {
            return Ok(());
        }

        if entry.huge(true) {
            let block_size = Self::level_size(level);
            if boundary.as_usize().is_multiple_of(block_size) {
                return Ok(());
            }

            let reserved = Self::new(self.allocator.clone())?;
            let reserved_paddr = reserved.paddr;
            let child_level = level - 1;
            if let Err(error) =
                self.split_huge_page_recursive(vaddr, level, reserved, HugeSplitFill::Inherit)
            {
                self.allocator.dealloc_frame(reserved_paddr);
                return Err(error);
            }

            let installed = self.as_slice()[index];
            let mut child = Self::from_paddr(installed.paddr(true), self.allocator.clone());
            child.split_leaf_for_boundary(vaddr, boundary, child_level)
        } else if !entry.present() {
            Ok(())
        } else {
            let mut child = Self::from_paddr(entry.paddr(true), self.allocator.clone());
            child.split_leaf_for_boundary(vaddr, boundary, level - 1)
        }
    }

    /// Commits a break-before-make split using a pre-zeroed child-table frame.
    ///
    /// The reserved frame is installed only on success. The caller remains
    /// responsible for reclaiming it after an error. No allocation occurs in
    /// the clear/flush/install window.
    pub(crate) fn split_huge_page_recursive(
        &mut self,
        vaddr: VirtAddr,
        level: usize,
        mut reserved: Frame<T, A>,
        fill: HugeSplitFill,
    ) -> PagingResult<(PhysAddr, PteConfigOf<T>, usize)> {
        debug_assert_eq!(reserved.frames, 1);
        let index = Self::virt_to_index(vaddr, level);
        let entry = self.as_slice()[index];
        if entry.unused() {
            return Err(PagingError::not_mapped());
        }
        let is_dir = level > 1;
        if entry.huge(is_dir) {
            let block_paddr = entry.paddr(is_dir);
            let block_config = entry.config(is_dir);
            if fill == HugeSplitFill::Inherit {
                let child_level = level - 1;
                let child_is_huge = child_level > 1;
                let child_stride = Self::level_size(child_level);
                for (child_index, child_entry) in reserved.as_slice_mut().iter_mut().enumerate() {
                    let offset = child_index
                        .checked_mul(child_stride)
                        .ok_or_else(|| PagingError::address_overflow("huge split child offset"))?;
                    let child_paddr = block_paddr
                        .as_usize()
                        .checked_add(offset)
                        .map(PhysAddr::from_usize)
                        .ok_or_else(|| PagingError::address_overflow("huge split frame"))?;
                    *child_entry = T::P::new_page(child_paddr, block_config, child_is_huge);
                }
            }

            // The caller serializes the page-table structure. Clearing before
            // the local invalidation prevents a walker from observing both the
            // old block descriptor and the new table descriptor.
            self.as_slice_mut()[index].clear();
            T::flush(Some(vaddr));
            self.as_slice_mut()[index] = T::P::new_table(reserved.paddr);
            return Ok((block_paddr, block_config, Self::level_size(level)));
        }
        if level == 1 || !entry.present() {
            return Err(PagingError::not_mapped());
        }

        let mut child = Self::from_paddr(entry.paddr(is_dir), self.allocator.clone());
        child.split_huge_page_recursive(vaddr, level - 1, reserved, fill)
    }

    /// Withdraws the child table installed by a huge split and restores the
    /// original block descriptor.  Validation is completed before the
    /// break-before-make sequence, so an error leaves the tree untouched.
    pub(crate) fn restore_huge_page_recursive(
        &mut self,
        block_vaddr: VirtAddr,
        block_paddr: PhysAddr,
        block_config: PteConfigOf<T>,
        block_size: usize,
        child_table_paddr: PhysAddr,
        level: usize,
    ) -> PagingResult<Frame<T, A>> {
        let index = Self::virt_to_index(block_vaddr, level);
        let entry = self.as_slice()[index];
        let is_dir = level > 1;

        if Self::level_size(level) == block_size {
            if !is_dir
                || !entry.present()
                || entry.huge(true)
                || entry.paddr(true) != child_table_paddr
            {
                return Err(PagingError::stale_huge_split(block_vaddr));
            }

            let child = Self::from_paddr(child_table_paddr, self.allocator.clone());
            self.as_slice_mut()[index].clear();
            T::flush(Some(block_vaddr));
            self.as_slice_mut()[index] = T::P::new_page(block_paddr, block_config, true);
            return Ok(child);
        }

        if level == 1 || !entry.present() || entry.huge(is_dir) {
            return Err(PagingError::stale_huge_split(block_vaddr));
        }
        let mut child = Self::from_paddr(entry.paddr(is_dir), self.allocator.clone());
        child.restore_huge_page_recursive(
            block_vaddr,
            block_paddr,
            block_config,
            block_size,
            child_table_paddr,
            level - 1,
        )
    }

    pub fn remap_recursive(
        &mut self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        config: PteConfigOf<T>,
        level: usize,
    ) -> PagingResult<usize> {
        let index = Self::virt_to_index(vaddr, level);
        let entry = self.as_slice()[index];
        if entry.unused() {
            return Err(PagingError::not_mapped());
        }
        let is_dir = level > 1;
        let is_huge = entry.huge(is_dir);
        if is_huge || level == 1 {
            let page_size = Self::level_size(level);
            let aligned_paddr = PhysAddr::from_usize(paddr.as_usize() & !(page_size - 1));
            self.as_slice_mut()[index] = T::P::new_page(aligned_paddr, config, is_huge);
            return Ok(page_size);
        }
        if !entry.present() {
            return Err(PagingError::not_mapped());
        }

        let mut child = Self::from_paddr(entry.paddr(is_dir), self.allocator.clone());
        child.remap_recursive(vaddr, paddr, config, level - 1)
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
        self.allocator
            .dealloc_frames(self.paddr, self.frames, T::PAGE_SIZE);
    }

    /// Detaches the current frame and all child page-table frames without
    /// releasing their physical storage.  `release` is called once per frame
    /// after its parent entry has been cleared.  This is the primitive used by
    /// address-space teardown to build a TLB quarantine.
    pub fn detach_recursive(
        &mut self,
        level: usize,
        release: &mut impl FnMut(DetachedPageTableFrame<A>),
    ) {
        self.detach_children(level, release);
        // `Frame` is a copyable view into the page table; transfer the allocator
        // clone to an owned token and leave no live owner in this view.
        release(DetachedPageTableFrame::new(
            self.paddr,
            self.frames,
            T::PAGE_SIZE,
            self.allocator.clone(),
        ));
    }

    fn detach_children(
        &mut self,
        level: usize,
        release: &mut impl FnMut(DetachedPageTableFrame<A>),
    ) {
        for i in (0..self.len()).rev() {
            let entry_info = {
                let entries = self.as_slice();
                if i < entries.len() {
                    let entry = entries[i];
                    (
                        entry.present(),
                        entry.huge(level > 1),
                        entry.paddr(level > 1),
                    )
                } else {
                    (false, false, crate::PhysAddr::from_usize(0))
                }
            };
            let (is_valid, is_huge, paddr) = entry_info;
            if !is_valid || is_huge || level == 1 {
                continue;
            }
            let mut child_frame = Frame::<T, A>::from_paddr(paddr, self.allocator.clone());
            child_frame.detach_recursive(level - 1, release);
            self.as_slice_mut()[i].clear();
        }
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
        for i in (0..self.len()).rev() {
            // 先获取当前PTE的状态
            let entry_info = {
                let entries = self.as_slice();
                if i < entries.len() {
                    let entry = entries[i];
                    (
                        entry.present(),
                        entry.huge(level > 1),
                        entry.paddr(level > 1),
                    )
                } else {
                    (false, false, crate::PhysAddr::from_usize(0))
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
                entries_mut[i].clear();
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
        let (pte, level) = self.find_occupied_leaf(vaddr, level)?;
        if !pte.present() {
            return Err(PagingError::not_mapped());
        }
        Ok((pte, level))
    }

    pub(crate) fn find_occupied_leaf(
        &self,
        vaddr: VirtAddr,
        level: usize,
    ) -> PagingResult<(T::P, usize)> {
        // 计算当前级别的页表索引
        let index = Self::virt_to_index(vaddr, level);

        // 获取页表项
        let entries = self.as_slice();
        let pte = entries[index];

        if pte.unused() {
            return Err(PagingError::not_mapped());
        }

        // 如果是大页映射或叶子级别，直接返回页表项及其级别
        if pte.huge(level > 1) || level == 1 {
            return Ok((pte, level));
        }

        // 否则，继续递归到下一级页表
        if level > 1 {
            if !pte.present() {
                return Err(PagingError::hierarchy_error(
                    "Non-present intermediate entry is not a leaf",
                ));
            }
            let child_frame: Frame<T, A> = Frame::from_pte(&pte, level, self.allocator.clone());
            return child_frame.find_occupied_leaf(vaddr, level - 1);
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
        if index >= self.len() || level <= 1 {
            return false;
        }

        let entries = self.as_slice();
        let entry = &entries[index];
        if entry.present() && !entry.huge(true) {
            // 递归释放子帧（子帧的级别是 level - 1）
            let mut child_frame = Frame::<T, A>::from_pte(entry, level, self.allocator.clone());
            child_frame.deallocate_recursive(level - 1);

            // 将当前PTE设为invalid
            let entries_mut = self.as_slice_mut();
            entries_mut[index].clear();

            true
        } else {
            false
        }
    }

    pub(crate) fn clone_entry_from(
        &mut self,
        source: &Self,
        index: usize,
        level: usize,
    ) -> PagingResult<bool> {
        if index >= self.len() || index >= source.len() {
            return Err(PagingError::hierarchy_error(
                "Entry index exceeds page-table frame size",
            ));
        }
        if !self.as_slice()[index].unused() {
            return Ok(false);
        }

        let source_entry = source.as_slice()[index];
        if source_entry.unused() {
            return Ok(false);
        }
        if level == 1 || source_entry.huge(true) {
            self.as_slice_mut()[index] = source_entry;
            return Ok(true);
        }
        if !source_entry.present() {
            return Err(PagingError::hierarchy_error(
                "Non-present intermediate entry is not a leaf",
            ));
        }

        let source_child = Self::from_paddr(source_entry.paddr(true), source.allocator.clone());
        let mut target_child = Self::new(self.allocator.clone())?;
        if let Err(err) = target_child.clone_children_from(&source_child, level - 1) {
            target_child.deallocate_recursive(level - 1);
            return Err(err);
        }

        self.as_slice_mut()[index] = T::P::new_table(target_child.paddr);
        Ok(true)
    }

    fn clone_children_from(&mut self, source: &Self, level: usize) -> PagingResult {
        for index in 0..source.len() {
            self.clone_entry_from(source, index, level)?;
        }
        Ok(())
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
