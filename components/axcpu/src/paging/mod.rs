mod arch;
#[cfg(any(target_pointer_width = "32", doc, docsrs))]
mod bits32;
#[cfg(any(target_pointer_width = "64", doc, docsrs))]
mod bits64;
pub mod entry;

use arrayvec::ArrayVec;
use ax_memory_addr::MemoryAddr;
pub use page_table_generic::{PageFrameProvider, PageSize, PagingError, PhysAddr, VirtAddr};

#[doc(no_inline)]
pub use self::entry::{GenericPTE, MappingFlags};
#[cfg(any(target_pointer_width = "32", doc, docsrs))]
pub use self::{
    arch::*,
    bits32::{PageTable32, PageTable32Cursor},
};
#[cfg(any(target_pointer_width = "64", doc, docsrs))]
pub use self::{
    arch::*,
    bits64::{PageTable64, PageTable64Cursor},
};

/// The specialized `Result` type for page table operations.
pub type PagingResult<T = ()> = Result<T, PagingError>;

/// Hardware scope of a host translation invalidation operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlbScope {
    /// The operation affects only the current processing element.
    Local,
    /// The architecture operation broadcasts within the shareable domain.
    HardwareBroadcast,
    /// The implementation explicitly sends and waits for remote IPIs.
    RemoteIpi,
}

/// Host architecture or runtime capability used to invalidate stale translations.
pub trait TlbInvalidator<A: MemoryAddr>: Sync + Send {
    /// Scope guaranteed by [`Self::invalidate`].
    const SCOPE: TlbScope;

    /// Invalidates one address, or the entire translation context for `None`.
    fn invalidate(vaddr: Option<A>);

    /// Invalidates a batch of individual addresses.
    fn invalidate_list(vaddrs: &[A]) {
        for &vaddr in vaddrs {
            Self::invalidate(Some(vaddr));
        }
    }
}

/// The **architecture-dependent** metadata that must be provided for
/// [`PageTable64`].
pub trait PagingMetaData: Sync + Send {
    /// The number of levels of the hardware page table.
    const LEVELS: usize;
    /// The maximum number of bits of physical address.
    const PA_MAX_BITS: usize;
    /// The maximum number of bits of virtual address.
    const VA_MAX_BITS: usize;

    /// The maximum physical address.
    const PA_MAX_ADDR: usize = (1 << Self::PA_MAX_BITS) - 1;

    /// The virtual address to be translated in this page table.
    ///
    /// This associated type allows more flexible use of page tables structs
    /// like [`PageTable64`], for example, to implement EPTs.
    type VirtAddr: MemoryAddr;
    /// Architecture TLB invalidation capability.
    type Tlb: TlbInvalidator<Self::VirtAddr>;
    // (^)it can be converted from/to usize and it's trivially copyable

    /// Whether a given physical address is valid.
    #[inline]
    fn paddr_is_valid(paddr: usize) -> bool {
        paddr <= Self::PA_MAX_ADDR // default
    }

    /// Whether a given virtual address is valid.
    #[inline]
    fn vaddr_is_valid(vaddr: usize) -> bool {
        // default: top bits sign extended
        let top_mask = usize::MAX << (Self::VA_MAX_BITS - 1);
        (vaddr & top_mask) == 0 || (vaddr & top_mask) == top_mask
    }
}

/// Returns whether the configured invalidator is safe for an SMP address space.
pub const fn smp_invalidation_available<M: PagingMetaData>() -> bool {
    matches!(
        M::Tlb::SCOPE,
        TlbScope::HardwareBroadcast | TlbScope::RemoteIpi
    )
}

// Keep small TLB batches inline so page-table mutation never allocates heap
// memory; larger batches deliberately fall back to one full invalidation.
const SMALL_FLUSH_THRESHOLD: usize = 32;

enum TlbFlusher<M: PagingMetaData> {
    None,
    Array(ArrayVec<M::VirtAddr, SMALL_FLUSH_THRESHOLD>),
    Full,
}
