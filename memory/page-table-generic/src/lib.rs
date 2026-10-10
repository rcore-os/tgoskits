#![no_std]

use core::fmt::Debug;

mod def;
pub mod frame;
mod map;
mod table;
mod unmap;
mod walk;

pub use def::*;
pub use frame::{DetachedPageTableFrame, Frame};
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
        // A malformed frame count/stride must never wrap back into a live
        // allocation.  Allocator implementations cannot return an error from
        // this legacy hook, so stop before the first unrepresentable address;
        // callers using the fallible detached-frame API get the full checked
        // range validation before reaching this path.
        for i in 0..frames {
            let Some(offset) = i.checked_mul(frame_size) else {
                break;
            };
            let Some(address) = start.as_usize().checked_add(offset) else {
                break;
            };
            self.dealloc_frame(PhysAddr::from_usize(address));
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

    /// 刷新TLB
    fn flush(vaddr: Option<VirtAddr>);

    /// Completes invalidation for changed mapping/table descriptors in this
    /// metadata implementation's flush domain.
    ///
    /// Implementors supply the descriptor-publication and completion barriers
    /// required by their architecture. The default preserves the scope of
    /// `flush(Some(address))`; a local flush does not confirm remote CPUs.
    /// Immediate table-frame reclamation therefore requires exclusive hardware
    /// use of the table or an implementation covering every active user. Shared
    /// stage-1 tables must use deferred reclamation and an external shootdown.
    fn flush_batch(vaddrs: &[VirtAddr]) {
        for &vaddr in vaddrs {
            Self::flush(Some(vaddr));
        }
    }

    /// Completes invalidation after changes confined to leaf descriptors.
    ///
    /// No parent entry may have been linked or detached in this batch. The
    /// default retains the invalidation scope of [`Self::flush_batch`].
    fn flush_leaf_batch(vaddrs: &[VirtAddr]) {
        Self::flush_batch(vaddrs);
    }

    /// Completes invalidation after clearing an old descriptor and before
    /// installing its replacement.
    ///
    /// The implementation must satisfy its architecture's pre-make ordering
    /// in the required flush domain before this call returns. On failure the
    /// caller restores the original descriptor and does not install a new one.
    /// The default preserves the metadata's local flush domain. Reclaiming the
    /// old physical owner still requires separate shootdown confirmation.
    fn flush_before_make(vaddr: VirtAddr, page_size: usize) -> PagingResult {
        if page_size > Self::PAGE_SIZE {
            Self::flush(None);
        } else {
            Self::flush_leaf_batch(core::slice::from_ref(&vaddr));
        }
        Ok(())
    }

    /// Checks that the required pre-make invalidation domain is available.
    ///
    /// This runs before clearing the old descriptor, so an unsupported
    /// platform can return an error without changing the mapping. Once it
    /// succeeds, [`Self::flush_before_make`] still must return success before
    /// the replacement becomes valid.
    fn prepare_break_before_make() -> PagingResult {
        Ok(())
    }

    /// Completes publication of a replacement leaf after break-before-make.
    ///
    /// The old leaf must already have been cleared and invalidated through
    /// [`Self::flush_before_make`] before the new descriptor was written.
    /// Architectures that can cache a translation fault still need to
    /// invalidate that cached result after the write.
    fn complete_replaced_leaf(vaddr: VirtAddr) {
        Self::flush_leaf_batch(core::slice::from_ref(&vaddr));
    }

    /// Completes publication of a newly installed descriptor.
    ///
    /// The caller has excluded concurrent software mutation. For a replacement,
    /// [`Self::flush_before_make`] has completed the required pre-make ordering;
    /// this hook then publishes the new descriptor. Architectures that can
    /// retain an invalid translation must also invalidate it after the write.
    /// This does not confirm remote revocation or permit owner reclamation;
    /// those still require the caller's shootdown. The default retains the
    /// metadata's batch invalidation scope.
    fn publish_new_mapping(vaddr: VirtAddr) {
        Self::flush_batch(core::slice::from_ref(&vaddr));
    }
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

    /// Whether replacing this leaf needs the old descriptor cleared and
    /// invalidated before the new descriptor is installed.
    ///
    /// Physical replacement requires this ordering by default. A descriptor
    /// format may also require it for changes to memory type, shareability,
    /// or translation scope even when the physical address stays the same.
    fn requires_break_before_make(&self, replacement: &Self, is_dir: bool) -> bool {
        self.paddr(is_dir) != replacement.paddr(is_dir)
    }

    /// Decodes the owner-defined leaf configuration.
    fn config(&self, is_dir: bool) -> Self::PteConfig;

    /// Returns whether this entry participates in address translation.
    ///
    /// Implementations must recognize both leaf mappings and child-table entries.
    fn present(&self) -> bool;

    /// Returns whether this entry is a block mapping at the current level.
    ///
    /// CPU page-table formats should preserve this structural answer for a
    /// retained non-present block. Formats that encode an empty-permission
    /// block as zero may return `false`; typed split then reports `NotMapped`.
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
