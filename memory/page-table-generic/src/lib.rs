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

/// The opaque leaf-entry configuration used by a page-table metadata type.
pub type PteConfigOf<T> = <<T as TableMeta>::P as PageTableEntry>::PteConfig;

pub trait FrameAllocator: Clone + Sync + Send + 'static {
    fn alloc_frame(&self) -> Option<PhysAddr>;

    fn dealloc_frame(&self, frame: PhysAddr);

    fn phys_to_virt(&self, paddr: PhysAddr) -> *mut u8;

    fn alloc_frames(&self, frames: usize, _align: usize) -> Option<PhysAddr> {
        if frames == 1 {
            self.alloc_frame()
        } else {
            None
        }
    }

    fn dealloc_frames(&self, start: PhysAddr, frames: usize, frame_size: usize) {
        if frames == 1 {
            self.dealloc_frame(start);
            return;
        }
        for i in 0..frames {
            self.dealloc_frame(PhysAddr::from_usize(start.as_usize() + i * frame_size));
        }
    }
}

pub trait TableMeta: Sync + Send + Clone + Copy + 'static {
    type P: PageTableEntry;

    /// 页面大小（支持4KB、16KB、64KB等）
    const PAGE_SIZE: usize;

    /// 各级索引位数数组，从最高级到最低级
    const LEVEL_BITS: &[usize];

    /// 大页最高支持的级别
    const MAX_BLOCK_LEVEL: usize;

    /// Whether addresses must fit the address width described by [`LEVEL_BITS`].
    const STRICT_ADDRESS_WIDTH: bool = false;

    /// Converts an address reconstructed from page-table indexes into the
    /// architecture's virtual-address representation.
    fn canonicalize_vaddr(vaddr: VirtAddr) -> VirtAddr {
        vaddr
    }

    /// Invalidate the TLB entry for `vaddr` (or the whole TLB when `None`).
    ///
    /// # SMP shootdown contract (intermediate-table break-before-free)
    ///
    /// [`PageTable::unmap`](crate::PageTable::unmap) reclaims an emptied
    /// intermediate page-table frame by clearing its parent descriptor, calling
    /// `flush(Some(vaddr))`, and then returning that frame to the allocator. For
    /// this to be sound on a multiprocessor, the `flush` at that point MUST be a
    /// *synchronous, system-wide* invalidation: once it returns, no other CPU
    /// running this address space may still walk the cleared descriptor or reuse
    /// a cached translation that reaches the freed frame. Otherwise a remote core
    /// can table-walk the frame after it has been reallocated and overwritten,
    /// translating to arbitrary physical memory.
    ///
    /// AArch64 `TLBI …; DSB` broadcasts to all PEs and satisfies this natively.
    /// Architectures whose native invalidation is local to the issuing hart
    /// (RISC-V `sfence.vma`, x86 `invlpg`) MUST turn this into a remote shootdown
    /// here — an IPI to every CPU sharing the address space plus a wait for its
    /// completion — or drive reclaim with `flush: false` and free the reclaimed
    /// frames externally only after performing that shootdown themselves. A
    /// hart-local flush alone is NOT a valid break-before-free completion barrier.
    fn flush(vaddr: Option<VirtAddr>);
}

pub trait PageTableEntry: Debug + Sync + Send + Clone + Copy + Sized + 'static {
    /// Configuration understood by this concrete PTE format.
    type PteConfig: Copy;

    /// Creates a leaf or block entry.
    fn new_page(paddr: PhysAddr, config: Self::PteConfig, is_huge: bool) -> Self;

    /// Creates an entry that points to a child page-table frame.
    fn new_table(paddr: PhysAddr) -> Self;

    /// Returns the physical address encoded by this entry.
    ///
    /// `is_dir` lets formats with level-dependent layouts decode the address
    /// without exposing those layout rules to the generic walker.
    fn paddr(&self, is_dir: bool) -> PhysAddr;

    /// Decodes the owner-defined leaf configuration.
    fn config(&self, is_dir: bool) -> Self::PteConfig;

    /// Returns whether this entry participates in address translation.
    ///
    /// Implementations must recognize both leaf mappings and child-table entries.
    fn present(&self) -> bool;

    /// Returns whether this entry is a block mapping at the current level.
    fn huge(&self, is_dir: bool) -> bool;

    /// Returns whether this entry contains no descriptor state at all.
    ///
    /// This is distinct from [`Self::present`]: a non-present leaf may retain its
    /// physical address so that a later protection change can activate it.
    fn unused(&self) -> bool;

    /// Clears all descriptor state from this entry.
    fn clear(&mut self);
}

pub trait PageTableOp: Send + 'static {
    type PteConfig: Copy;

    fn addr(&self) -> PhysAddr;
    fn map(&mut self, config: &MapConfig<Self::PteConfig>) -> PagingResult;
    fn unmap(&mut self, virt_start: VirtAddr, size: usize) -> Result<(), PagingError>;
}

impl<T: TableMeta, A: FrameAllocator> PageTableOp for PageTable<T, A> {
    type PteConfig = PteConfigOf<T>;

    fn addr(&self) -> PhysAddr {
        self.root_paddr()
    }

    fn map(&mut self, config: &MapConfig<Self::PteConfig>) -> PagingResult {
        PageTableRef::map(self, config)
    }

    fn unmap(&mut self, virt_start: VirtAddr, size: usize) -> PagingResult {
        PageTableRef::unmap(self, virt_start, size)
    }
}

impl<T: TableMeta, A: FrameAllocator> PageTableOp for PageTableRef<T, A> {
    type PteConfig = PteConfigOf<T>;

    fn addr(&self) -> PhysAddr {
        self.root_paddr()
    }

    fn map(&mut self, config: &MapConfig<Self::PteConfig>) -> PagingResult {
        self.map(config)
    }

    fn unmap(&mut self, virt_start: VirtAddr, size: usize) -> Result<(), PagingError> {
        self.unmap(virt_start, size)
    }
}
