use core::ops::{Deref, DerefMut};

use crate::{
    FrameAllocator, PageTableEntry, PagingError, PagingResult, PhysAddr, TableMeta, VirtAddr,
    frame::Frame,
    map::{MapConfig, MapRecursiveConfig, UnmapConfig, UnmapRecursiveConfig},
    walk::{PageTableWalker, WalkConfig},
};

pub struct PageTable<T: TableMeta, A: FrameAllocator> {
    inner: PageTableRef<T, A>,
}

impl<T: TableMeta, A: FrameAllocator> PageTable<T, A> {
    pub const VALID_BITS: usize = Frame::<T, A>::PT_VALID_BITS;

    /// 创建一个新的页表
    pub fn new(allocator: A) -> PagingResult<Self> {
        let inner = unsafe { PageTableRef::new(allocator) }?;
        Ok(Self { inner })
    }

    pub fn valid_bits(&self) -> usize {
        Frame::<T, A>::PT_VALID_BITS
    }
}

impl<T: TableMeta, A: FrameAllocator> Drop for PageTable<T, A> {
    fn drop(&mut self) {
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
            .field("root_paddr", &format_args!("{:#x}", self.root.paddr.raw()))
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
        let root = Frame::new(allocator)?;
        Ok(Self { root })
    }

    pub fn from_paddr(paddr: PhysAddr, allocator: A) -> Self {
        let root = Frame::from_paddr(paddr, allocator);
        Self { root }
    }

    /// 映射虚拟地址范围到物理地址范围
    pub fn map(&mut self, config: &MapConfig) -> PagingResult {
        // 验证输入参数
        self.validate_map_config(config)?;

        // 检查大小溢出
        if config.vaddr.raw().checked_add(config.size).is_none()
            || config.paddr.raw().checked_add(config.size).is_none()
        {
            return Err(PagingError::address_overflow(
                "Virtual or physical address overflow",
            ));
        }

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
        let end_vaddr: VirtAddr = match start_vaddr.raw().checked_add(size) {
            Some(end) => VirtAddr::new(end),
            None => {
                return Err(PagingError::address_overflow(
                    "Virtual address overflow in unmap",
                ));
            }
        };

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

        let end_vaddr = match config.start_vaddr.raw().checked_add(config.size) {
            Some(end) => VirtAddr::new(end),
            None => {
                return Err(PagingError::address_overflow(
                    "Virtual address overflow in unmap_with_config",
                ));
            }
        };

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
        if !start_vaddr.raw().is_multiple_of(T::PAGE_SIZE) {
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
        PageTableWalker::new(self, config).filter(|p| p.pte.to_config(false).valid)
    }

    /// 遍历所有有效的最终映射页表项（过滤掉无效项和中间级别的页表指针）
    pub fn walk_valid(&self) -> impl Iterator<Item = crate::walk::PteInfo<T::P>> + '_ {
        self.walk(0.into(), usize::MAX.into()).filter(|p| {
            let config = p.pte.to_config(false);
            config.valid && p.is_final_mapping
        })
    }

    /// 验证映射配置的有效性
    fn validate_map_config(&self, config: &MapConfig) -> PagingResult {
        if config.size == 0 {
            return Err(PagingError::invalid_size("Size cannot be zero"));
        }

        // 检查虚拟地址和物理地址是否页对齐
        if !config.vaddr.raw().is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Virtual address not page aligned",
            ));
        }

        if !config.paddr.raw().is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Physical address not page aligned",
            ));
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
        let (pte, level) = self
            .root
            .translate_recursive_with_level(vaddr, Frame::<T, A>::PT_LEVEL)?;

        let pte_config = pte.to_config(level > 1);

        // 根据页表项类型计算正确的偏移
        let (phys_addr, _) = if pte_config.huge {
            // 大页映射：需要使用实际级别的大小来计算偏移
            let level_size = Frame::<T, A>::level_size(level);
            let offset_in_page = vaddr.raw() % level_size;
            (
                PhysAddr::new(pte_config.paddr.raw() + offset_in_page),
                level_size,
            )
        } else {
            // 普通页面映射：使用页面大小
            let offset_in_page = vaddr.raw() % T::PAGE_SIZE;
            (
                PhysAddr::new(pte_config.paddr.raw() + offset_in_page),
                T::PAGE_SIZE,
            )
        };

        Ok((phys_addr, pte))
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
