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

pub trait FramAllocator: Clone + Copy + Sync + Send + 'static {
    fn alloc_frame(&self) -> Option<PhysAddr>;

    fn dealloc_frame(&self, frame: PhysAddr);

    fn phys_to_virt(&self, paddr: PhysAddr) -> *mut u8;
}

pub trait TableGeneric: Sync + Send + Clone + Copy + 'static {
    type P: PageTableEntry;

    /// 页面大小（支持4KB、16KB、64KB等）
    const PAGE_SIZE: usize;
    /// 页表级别数（支持3级、4级、5级等）
    const LEVEL: usize;
    /// 大页最高支持的级别
    const MAX_BLOCK_LEVEL: usize;

    const INDEX_BITS: usize = 9;

    /// 有效地址位数
    const VALID_BITS: usize =
        Self::PAGE_SIZE.trailing_zeros() as usize + Self::LEVEL * Self::INDEX_BITS;
    /// 每个页表的条目数
    const TABLE_LEN: usize = Self::PAGE_SIZE / core::mem::size_of::<Self::P>();

    /// 刷新TLB
    fn flush(vaddr: Option<VirtAddr>);
}

pub trait PageTableEntry: Debug + Sync + Send + Clone + Copy + Sized + 'static {
    fn valid(&self) -> bool;
    fn paddr(&self) -> PhysAddr;
    fn set_paddr(&mut self, paddr: PhysAddr);
    fn set_valid(&mut self, valid: bool);
    fn is_huge(&self) -> bool;
    fn set_is_huge(&mut self, b: bool);
}
