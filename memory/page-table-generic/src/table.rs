use core::ops::{Deref, DerefMut, Range};

use ax_memory_addr::MemoryAddr;

use crate::{
    FrameAllocator, PageTableEntry, PagingError, PagingResult, PhysAddr, PteConfigOf, TableMeta,
    VirtAddr,
    frame::Frame,
    map::{MapConfig, MapRecursiveConfig, UnmapConfig, UnmapRecursiveConfig},
    walk::{PageTableWalker, WalkConfig},
};

const TARGETED_FLUSH_LIMIT: usize = 32;

pub struct PageTable<T: TableMeta, A: FrameAllocator> {
    inner: PageTableRef<T, A>,
    #[cfg(feature = "copy-from")]
    borrowed_root_entries: Option<Range<usize>>,
}

impl<T: TableMeta, A: FrameAllocator> PageTable<T, A> {
    pub const VALID_BITS: usize = Frame::<T, A>::PT_VALID_BITS;

    /// 创建一个新的页表
    pub fn new(allocator: A) -> PagingResult<Self> {
        let inner = unsafe { PageTableRef::new(allocator) }?;
        Ok(Self {
            inner,
            #[cfg(feature = "copy-from")]
            borrowed_root_entries: None,
        })
    }

    pub const fn root_paddr(&self) -> PhysAddr {
        self.inner.root.paddr
    }

    /// Deep-copies source root entries that are absent from this page table.
    ///
    /// Leaf mappings keep referring to the same physical memory, while every
    /// copied intermediate page-table frame is independently owned by this
    /// page table. Existing destination root entries are left unchanged.
    ///
    /// If allocation fails, entries copied before the failure remain installed
    /// and are reclaimed normally when this page table is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the range overflows or wraps around the root table,
    /// or if an intermediate page-table frame cannot be allocated.
    pub fn clone_missing_root_entries_from(
        &mut self,
        other: &PageTableRef<T, A>,
        start_vaddr: VirtAddr,
        size: usize,
    ) -> PagingResult {
        let Some(entries) = Self::root_entry_range(start_vaddr, size)? else {
            return Ok(());
        };

        let root_level = Frame::<T, A>::PT_LEVEL;
        let mut changed = false;
        for index in entries {
            changed |= self
                .inner
                .root
                .clone_entry_from(&other.root, index, root_level)?;
        }
        if changed {
            T::flush(None);
        }
        Ok(())
    }

    /// Shares root page-table entries from another page table.
    ///
    /// Mappings below the shared root entries remain owned by the source and
    /// changes made there are visible through both page tables.
    ///
    /// # Safety
    ///
    /// The source page table must outlive this page table. The caller must
    /// also prevent this page table from modifying or unmapping the shared
    /// virtual-address range.
    #[cfg(feature = "copy-from")]
    pub unsafe fn share_root_entries_from(
        &mut self,
        other: &Self,
        start_vaddr: VirtAddr,
        size: usize,
    ) -> PagingResult {
        if size == 0 {
            return Ok(());
        }
        if self.borrowed_root_entries.is_some() {
            return Err(PagingError::hierarchy_error(
                "Page table already contains shared root entries",
            ));
        }

        let Some(entries) = Self::root_entry_range(start_vaddr, size)? else {
            return Ok(());
        };
        let root_level = Frame::<T, A>::PT_LEVEL;

        for index in entries.clone() {
            self.inner.root.dealloc_entry_recursive(index, root_level);
            self.inner.root.as_slice_mut()[index] = other.inner.root.as_slice()[index];
        }
        self.borrowed_root_entries = Some(entries);
        T::flush(None);
        Ok(())
    }

    fn root_entry_range(start_vaddr: VirtAddr, size: usize) -> PagingResult<Option<Range<usize>>> {
        if size == 0 {
            return Ok(None);
        }
        let end_vaddr = start_vaddr
            .as_usize()
            .checked_add(size)
            .ok_or_else(|| PagingError::address_overflow("root_entry_range"))?;
        let root_level = Frame::<T, A>::PT_LEVEL;
        let start_index = Frame::<T, A>::virt_to_index(start_vaddr, root_level);
        let end_index =
            Frame::<T, A>::virt_to_index(VirtAddr::from_usize(end_vaddr - 1), root_level) + 1;
        if start_index >= end_index {
            return Err(PagingError::invalid_range(
                "Range must be contiguous in the root page table",
            ));
        }
        Ok(Some(start_index..end_index))
    }

    #[cfg(feature = "copy-from")]
    fn detach_borrowed_root_entries(&mut self) {
        let Some(entries) = self.borrowed_root_entries.take() else {
            return;
        };
        for index in entries {
            self.inner.root.as_slice_mut()[index].clear();
        }
    }
}

impl<T: TableMeta, A: FrameAllocator> Drop for PageTable<T, A> {
    fn drop(&mut self) {
        #[cfg(feature = "copy-from")]
        self.detach_borrowed_root_entries();
        unsafe {
            // 释放所有页表帧，但不释放映射的物理页
            self.deallocate();
        }
    }
}

impl<T: TableMeta, A: FrameAllocator> Deref for PageTable<T, A> {
    type Target = PageTableRef<T, A>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: TableMeta, A: FrameAllocator> DerefMut for PageTable<T, A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[derive(Clone, Copy)]
pub struct PageTableRef<T: TableMeta, A: FrameAllocator> {
    pub root: Frame<T, A>,
}

impl<T: TableMeta, A: FrameAllocator> core::fmt::Debug for PageTableRef<T, A>
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

impl<T: TableMeta, A: FrameAllocator> PageTableRef<T, A> {
    /// 创建一个新的页表
    ///
    /// # Safety
    ///
    /// 调用者必须确保提供的FrameAllocator是有效的，并且在页表生命周期内保持有效
    pub unsafe fn new(allocator: A) -> PagingResult<Self> {
        let root = Frame::new_root(allocator)?;
        Ok(Self { root })
    }

    pub fn from_paddr(paddr: PhysAddr, allocator: A) -> Self {
        let root = Frame::from_root_paddr(paddr, allocator);
        Self { root }
    }

    /// Maps one page with the requested page size.
    pub fn map_page(
        &mut self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        page_size: usize,
        config: PteConfigOf<T>,
    ) -> PagingResult {
        let Some(level) = Frame::<T, A>::level_for_page_size(page_size) else {
            return Err(PagingError::invalid_size(
                "Page size is not represented by the page-table levels",
            ));
        };
        if level > 1 && level > T::MAX_BLOCK_LEVEL {
            return Err(PagingError::invalid_size(
                "Page size exceeds the architecture's block-mapping level",
            ));
        }
        self.map(&MapConfig {
            vaddr: vaddr.align_down(page_size),
            paddr: paddr.align_down(page_size),
            size: page_size,
            pte: config,
            allow_huge: level > 1,
            flush: true,
        })
    }

    /// Maps a contiguous virtual region, choosing large pages when possible.
    /// Mappings installed by this call are rolled back if a later page fails.
    /// TLB invalidation is deferred and batched until the region has been updated.
    pub fn map_region(
        &mut self,
        start_vaddr: VirtAddr,
        get_paddr: impl Fn(VirtAddr) -> PhysAddr,
        size: usize,
        config: PteConfigOf<T>,
        allow_huge: bool,
    ) -> PagingResult {
        if size == 0 {
            return Err(PagingError::invalid_size("Region size cannot be zero"));
        }
        if !start_vaddr.as_usize().is_multiple_of(T::PAGE_SIZE)
            || !size.is_multiple_of(T::PAGE_SIZE)
        {
            return Err(PagingError::alignment_error(
                "Region start and size must be base-page aligned",
            ));
        }
        start_vaddr.as_usize().checked_add(size).ok_or_else(|| {
            PagingError::address_overflow("Virtual address overflow in map_region")
        })?;
        self.validate_address_width(start_vaddr, size, "map_region")?;

        let mut offset = 0;
        let mut flush_addrs = heapless::Vec::<VirtAddr, TARGETED_FLUSH_LIMIT>::new();
        let mut full_flush = false;
        let result = loop {
            if offset >= size {
                break Ok(());
            }
            let vaddr = start_vaddr + offset;
            let paddr = get_paddr(vaddr);
            let remaining = size - offset;
            let page_size = largest_page_size::<T, A>(vaddr, paddr, remaining, allow_huge);
            if let Err(err) = self.map(&MapConfig {
                vaddr: vaddr.align_down(page_size),
                paddr: paddr.align_down(page_size),
                size: page_size,
                pte: config,
                allow_huge: page_size > T::PAGE_SIZE,
                flush: false,
            }) {
                let rollback_result = if offset == 0 {
                    Ok(())
                } else {
                    self.unmap_with_config(&UnmapConfig {
                        start_vaddr,
                        size: offset,
                        flush: false,
                    })
                };
                break match rollback_result {
                    Ok(()) => Err(err),
                    Err(rollback_err) => Err(rollback_err),
                };
            }
            if !full_flush && flush_addrs.push(vaddr).is_err() {
                full_flush = true;
                flush_addrs.clear();
            }
            offset += page_size;
        };

        if full_flush {
            T::flush(None);
        } else {
            for vaddr in flush_addrs {
                T::flush(Some(vaddr));
            }
        }
        result
    }

    /// Unmaps one page and returns its physical address, flags, and page size.
    pub fn unmap_page(
        &mut self,
        vaddr: VirtAddr,
    ) -> PagingResult<(PhysAddr, PteConfigOf<T>, usize)> {
        let (pte, level) = self
            .root
            .find_occupied_leaf(vaddr, Frame::<T, A>::PT_LEVEL)?;
        let page_size = Frame::<T, A>::level_size(level);
        let is_dir = level > 1;
        let paddr = pte.paddr(is_dir);
        let config = pte.config(is_dir);
        self.unmap_with_config(&UnmapConfig {
            start_vaddr: vaddr.align_down(page_size),
            size: page_size,
            flush: true,
        })?;
        Ok((paddr, config, page_size))
    }

    /// Changes one existing mapping's flags and returns its page size.
    pub fn protect_page(&mut self, vaddr: VirtAddr, config: PteConfigOf<T>) -> PagingResult<usize> {
        let page_size = self
            .root
            .protect_recursive(vaddr, config, Frame::<T, A>::PT_LEVEL)?;
        T::flush(Some(vaddr));
        Ok(page_size)
    }

    /// Changes flags for a region. Unmapped base pages are skipped.
    pub fn protect_region(
        &mut self,
        start_vaddr: VirtAddr,
        size: usize,
        config: PteConfigOf<T>,
    ) -> PagingResult {
        let end = start_vaddr
            .as_usize()
            .checked_add(size)
            .ok_or_else(|| PagingError::address_overflow("protect_region"))?;
        let mut vaddr = start_vaddr;
        while vaddr.as_usize() < end {
            match self.protect_page(vaddr, config) {
                Ok(page_size) => vaddr += page_size,
                Err(PagingError::NotMapped) => vaddr += T::PAGE_SIZE,
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    /// Remaps one existing mapping and returns its page size.
    pub fn remap_page(
        &mut self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        config: PteConfigOf<T>,
    ) -> PagingResult<usize> {
        let page_size = self
            .root
            .remap_recursive(vaddr, paddr, config, Frame::<T, A>::PT_LEVEL)?;
        T::flush(Some(vaddr));
        Ok(page_size)
    }

    /// Queries one mapping and returns the translated physical address, flags, and page size.
    pub fn query(&self, vaddr: VirtAddr) -> PagingResult<(PhysAddr, PteConfigOf<T>, usize)> {
        let (paddr, pte, level) = self.translate_with_level(vaddr)?;
        Ok((
            paddr,
            pte.config(level > 1),
            Frame::<T, A>::level_size(level),
        ))
    }

    /// 映射虚拟地址范围到物理地址范围
    pub fn map(&mut self, config: &MapConfig<PteConfigOf<T>>) -> PagingResult {
        // 验证输入参数
        self.validate_map_config(config)?;

        // 检查大小溢出
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

    /// 取消映射虚拟地址范围
    ///
    /// # 参数
    /// - `start_vaddr`: 要取消映射的起始虚拟地址
    /// - `size`: 要取消映射的大小（字节）
    ///
    /// # 返回值
    /// - `Ok(())`: 取消映射成功
    /// - `Err(PagingError)`: 取消映射失败
    ///
    /// # 行为
    /// - 清除指定虚拟地址范围内的所有页表项
    /// - 自动回收空的子页表帧
    /// - 支持大页和普通页面的取消映射
    /// - 根据配置刷新TLB
    pub fn unmap(&mut self, start_vaddr: VirtAddr, size: usize) -> PagingResult<()> {
        // 验证输入参数
        self.validate_unmap_params(start_vaddr, size)?;

        // 检查大小溢出
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
            flush: true, // 默认刷新TLB确保一致性
        })?;

        Ok(())
    }

    /// 使用配置对象取消映射
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

    /// 验证取消映射参数的有效性
    fn validate_unmap_params(&self, start_vaddr: VirtAddr, size: usize) -> PagingResult<()> {
        if size == 0 {
            return Err(PagingError::invalid_size("Size cannot be zero in unmap"));
        }

        // 检查虚拟地址是否页对齐
        if !start_vaddr.as_usize().is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Start virtual address not page aligned in unmap",
            ));
        }

        // 检查大小是否页对齐
        if !size.is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Size not page aligned in unmap",
            ));
        }

        Ok(())
    }

    /// 创建页表遍历迭代器
    pub fn walk_all(&self, config: WalkConfig) -> PageTableWalker<'_, T, A> {
        PageTableWalker::new(self, config)
    }

    pub fn walk(
        &self,
        start_vaddr: VirtAddr,
        end_vaddr: VirtAddr,
    ) -> impl Iterator<Item = crate::walk::PteInfo<T::P>> + '_ {
        let config = WalkConfig {
            start_vaddr,
            end_vaddr,
        };
        PageTableWalker::new(self, config).filter(|p| p.pte.present())
    }

    /// 遍历所有有效的最终映射页表项（过滤掉无效项和中间级别的页表指针）
    pub fn walk_valid(&self) -> impl Iterator<Item = crate::walk::PteInfo<T::P>> + '_ {
        self.walk(0.into(), usize::MAX.into())
            .filter(|p| p.pte.present() && p.is_final_mapping)
    }

    /// 验证映射配置的有效性
    fn validate_map_config(&self, config: &MapConfig<PteConfigOf<T>>) -> PagingResult {
        if config.size == 0 {
            return Err(PagingError::invalid_size("Size cannot be zero"));
        }

        // 检查虚拟地址和物理地址是否页对齐
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

    pub const fn page_size() -> usize {
        T::PAGE_SIZE
    }

    pub const fn table_levels() -> usize {
        T::LEVEL_BITS.len()
    }

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

    /// 销毁整个页表结构
    ///
    /// 此方法会：
    /// 1. 递归释放根帧及所有子页表帧
    /// 2. 清除所有页表项（设为invalid）
    /// 3. 不释放映射的物理页（数据页/大页）
    ///
    /// # Safety
    /// 调用者必须确保：
    /// - 没有其他代码在访问这个页表
    /// - 没有CPU正在使用这个页表进行地址翻译
    /// - 调用后不再使用这个PageTable实例
    pub unsafe fn destroy(mut self) {
        self.root.deallocate_recursive(Frame::<T, A>::PT_LEVEL);
    }

    /// 释放页表占用的所有页表帧
    ///
    /// 与destroy()不同，这个方法保留PageTable结构，
    /// 但释放所有关联的页表帧。调用后PageTable不再可用。
    ///
    /// 释放行为：
    /// - 释放所有页表帧
    /// - 清除所有页表项（设为invalid）
    /// - 不释放映射的物理页（数据页/大页）
    ///
    /// # Safety
    /// 调用者必须确保：
    /// - 没有其他代码在访问这个页表
    /// - 没有CPU正在使用这个页表进行地址翻译
    pub unsafe fn deallocate(&mut self) {
        self.root.deallocate_recursive(Frame::<T, A>::PT_LEVEL);
    }

    /// 释放页表中的指定映射区域
    ///
    /// 释放指定虚拟地址范围内的所有页表项和子页表帧
    /// 在释放前将相关PTE设为invalid
    pub fn deallocate_range(&mut self, start_vaddr: VirtAddr, end_vaddr: VirtAddr) -> PagingResult {
        if start_vaddr >= end_vaddr {
            return Err(PagingError::invalid_range(
                "Start address must be less than end address",
            ));
        }

        // TODO: 实现范围释放逻辑
        // 这里需要实现：
        // 1. 遍历指定虚拟地址范围
        // 2. 释放对应的页表项和子页表
        // 3. 处理部分页表项的情况

        Ok(())
    }

    /// 通过虚拟地址查询页表项
    ///
    /// # 参数
    /// - `vaddr`: 要查询的虚拟地址
    ///
    /// # 返回值
    /// - `Ok(T::P)`: 找到的页表项，包含物理地址信息
    /// - `Err(PagingError)`: 查询失败，原因可能包括：
    ///   - 地址未映射
    ///   - 页表项无效
    ///   - 页表层次结构错误
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

        let is_huge = pte.huge(level > 1);
        let pte_paddr = pte.paddr(level > 1);

        // 根据页表项类型计算正确的偏移
        let (phys_addr, _) = if is_huge {
            // 大页映射：需要使用实际级别的大小来计算偏移
            let level_size = Frame::<T, A>::level_size(level);
            let offset_in_page = vaddr.as_usize() % level_size;
            (
                PhysAddr::from_usize(pte_paddr.as_usize() + offset_in_page),
                level_size,
            )
        } else {
            // 普通页面映射：使用页面大小
            let offset_in_page = vaddr.as_usize() % T::PAGE_SIZE;
            (
                PhysAddr::from_usize(pte_paddr.as_usize() + offset_in_page),
                T::PAGE_SIZE,
            )
        };

        Ok((phys_addr, pte, level))
    }

    /// 通过虚拟地址查询物理地址（便利方法）
    ///
    /// # 参数
    /// - `vaddr`: 要查询的虚拟地址
    ///
    /// # 返回值
    /// - `Ok(PhysAddr)`: 找到的物理地址
    /// - `Err(PagingError)`: 查询失败
    pub fn translate_phys(&self, vaddr: VirtAddr) -> PagingResult<PhysAddr> {
        let (p, _) = self.translate(vaddr)?;
        Ok(p)
    }

    /// 检查虚拟地址是否已映射
    ///
    /// 这是一个便利方法，用于快速检查地址是否已映射而不需要获取页表项
    ///
    /// # 参数
    /// - `vaddr`: 要检查的虚拟地址
    ///
    /// # 返回值
    /// - `true`: 地址已映射
    /// - `false`: 地址未映射
    pub fn is_mapped(&self, vaddr: VirtAddr) -> bool {
        self.translate(vaddr).is_ok()
    }

    /// 获取页表的根帧物理地址
    pub fn root_paddr(&self) -> crate::PhysAddr {
        self.root.paddr
    }
}

fn largest_page_size<T: TableMeta, A: FrameAllocator>(
    vaddr: VirtAddr,
    paddr: PhysAddr,
    remaining: usize,
    allow_huge: bool,
) -> usize {
    if allow_huge {
        let max_level = Frame::<T, A>::PT_LEVEL.min(T::MAX_BLOCK_LEVEL);
        for level in (2..=max_level).rev() {
            let page_size = Frame::<T, A>::level_size(level);
            if vaddr.is_aligned(page_size) && paddr.is_aligned(page_size) && remaining >= page_size
            {
                return page_size;
            }
        }
    }
    T::PAGE_SIZE
}
