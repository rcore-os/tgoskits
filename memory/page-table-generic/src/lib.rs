#![no_std]

use core::fmt::Debug;

mod def;
pub mod frame;
mod map;
mod table;
mod walk;

pub use def::*;
pub use frame::Frame;
pub use map::*;
pub use table::*;
pub use walk::*;

pub type PagingResult<T = ()> = Result<T, PagingError>;

pub trait FrameAllocator: Clone + Sync + Send + 'static {
    fn alloc_frame(&self) -> Option<PhysAddr>;

    fn dealloc_frame(&self, frame: PhysAddr);

    fn phys_to_virt(&self, paddr: PhysAddr) -> *mut u8;
}

pub trait TableMeta: Sync + Send + Clone + Copy + 'static {
    type P: PageTableEntry;

    /// 页面大小（支持4KB、16KB、64KB等）
    const PAGE_SIZE: usize;

    /// 各级索引位数数组，从最高级到最低级
    const LEVEL_BITS: &[usize];

    /// 大页最高支持的级别
    const MAX_BLOCK_LEVEL: usize;

    /// 刷新TLB
    fn flush(vaddr: Option<VirtAddr>);
}

pub trait PageTableEntry: Debug + Sync + Send + Clone + Copy + Sized + 'static {
    /// 从 PteConfig 创建页表项
    ///
    /// # 参数
    /// - `config`: 包含所有页表项配置的结构
    ///
    /// # 返回
    /// 新的页表项实例
    fn from_config(config: PteConfig) -> Self;

    /// 将页表项转换为 PteConfig
    ///
    /// # 参数
    /// - `is_dir`: 是否为目录项（影响物理地址布局解析）
    ///   - true: 目录项（可能包含大页映射或子页表指针）
    ///   - false: 页表项（叶子级别，基本页映射）
    ///
    /// # 返回
    /// 包含当前页表项所有状态的 PteConfig
    fn to_config(&self, is_dir: bool) -> PteConfig;

    fn valid(&self) -> bool;
}

pub trait PageTableOp: Send + 'static {
    fn addr(&self) -> PhysAddr;
    fn map(&mut self, config: &MapConfig) -> PagingResult;
    fn unmap(&mut self, virt_start: VirtAddr, size: usize) -> Result<(), PagingError>;
}

impl<T: TableMeta, A: FrameAllocator> PageTableOp for PageTable<T, A> {
    fn addr(&self) -> PhysAddr {
        self.root_paddr()
    }

    fn map(&mut self, config: &MapConfig) -> PagingResult {
        PageTableRef::map(self, config)
    }

    fn unmap(&mut self, virt_start: VirtAddr, size: usize) -> PagingResult {
        PageTableRef::unmap(self, virt_start, size)
    }
}

impl<T: TableMeta, A: FrameAllocator> PageTableOp for PageTableRef<T, A> {
    fn addr(&self) -> PhysAddr {
        self.root_paddr()
    }

    fn map(&mut self, config: &MapConfig) -> PagingResult {
        self.map(config)
    }

    fn unmap(&mut self, virt_start: VirtAddr, size: usize) -> Result<(), PagingError> {
        self.unmap(virt_start, size)
    }
}
