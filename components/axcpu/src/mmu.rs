//! Local address translation hardware operations.

#[cfg(feature = "uspace")]
pub use crate::arch::current::asm::install_user_address_space;
pub use crate::arch::current::asm::{
    address_space_tag_capacity, flush_tlb, read_kernel_page_table, read_user_page_table,
    write_kernel_page_table, write_user_page_table,
};

/// Hardware translation state installed while the caller owns its page tables.
/// Software identities, tag generations and mapping epochs belong to the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HardwareAddressSpace {
    root: ax_memory_addr::PhysAddr,
    tag: u16,
}

impl HardwareAddressSpace {
    /// Carries a page-table root and hardware tag; tag zero requests a full flush.
    /// Creating this value does not access or activate the page tables.
    pub const fn new(root: ax_memory_addr::PhysAddr, tag: u16) -> Self {
        Self { root, tag }
    }

    /// Returns the physical root address.
    pub const fn root(self) -> ax_memory_addr::PhysAddr {
        self.root
    }

    /// Returns the hardware tag, with zero reserved for the full-flush path.
    pub const fn hardware_tag(self) -> u16 {
        self.tag
    }
}

/// Invalidates every page touched by a byte range on the current CPU.
///
/// Zero length is a no-op. Unaligned endpoints include both boundary pages;
/// overflowing or sufficiently large ranges use full local invalidation.
/// Local IRQ exclusion keeps the bounded sequence on one CPU. Cross-CPU
/// shootdown, mapping generations and memory reclamation belong to the caller.
pub fn flush_tlb_range(start: crate::VirtAddr, size: usize) {
    flush_range_with(start, size, flush_tlb);
}

pub(crate) fn flush_range_with(
    start: crate::VirtAddr,
    size: usize,
    flush_tlb: impl Fn(Option<crate::VirtAddr>),
) {
    if size == 0 {
        return;
    }
    struct RestoreIrqs(bool);
    impl Drop for RestoreIrqs {
        fn drop(&mut self) {
            if self.0 {
                crate::interrupt::enable_irqs();
            }
        }
    }
    let _restore = RestoreIrqs(crate::interrupt::irqs_enabled());
    crate::interrupt::disable_irqs();
    let Some(last) = start.as_usize().checked_add(size - 1) else {
        flush_tlb(None);
        return;
    };
    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    let first = start.as_usize() & !(PAGE_SIZE - 1);
    let pages = (last - first) / PAGE_SIZE + 1;
    if pages > crate::arch::current::TLB_RANGE_PAGE_LIMIT {
        flush_tlb(None);
        return;
    }
    // Each base lies at or below the checked inclusive endpoint, so neither
    // the multiplication nor address addition can wrap in this bounded loop.
    for page in 0..pages {
        flush_tlb(Some(crate::VirtAddr::from_usize(first + page * PAGE_SIZE)));
    }
}

/// Publishes a local page-fault mapping before the faulting access is retried.
/// The address is normalized to the architecture's base-page boundary. This
/// does not provide cross-CPU invalidation or an address-space ownership token.
pub fn update_mmu_cache(vaddr: crate::VirtAddr) {
    let base = vaddr.as_usize() & !(ax_memory_addr::PAGE_SIZE_4K - 1);
    crate::arch::current::asm::update_mmu_cache(crate::VirtAddr::from_usize(base));
}

#[cfg(target_arch = "aarch64")]
pub use El1 as Native;

#[cfg(target_arch = "aarch64")]
pub use crate::arch::current::mmu::{El1, El2};

/// Native kernel translation regime selected by the target architecture.
#[cfg(not(target_arch = "aarch64"))]
pub struct Native;

#[cfg(not(target_arch = "aarch64"))]
impl Native {
    /// Returns the current kernel page-table root.
    pub fn read_kernel_page_table() -> crate::PhysAddr {
        read_kernel_page_table()
    }
    /// Installs a native kernel root without implicit invalidation.
    ///
    /// # Safety
    /// All current code, stack and data mappings must remain valid, and the
    /// owner must retain the tables and arrange required TLB invalidation.
    pub unsafe fn write_kernel_page_table(root: crate::PhysAddr) {
        // SAFETY: the caller retains the native address-space installation contract.
        unsafe { write_kernel_page_table(root) };
    }
    /// Invalidates a native translation or the entire native local TLB.
    pub fn flush_tlb(address: Option<crate::VirtAddr>) {
        flush_tlb(address);
    }
    /// Invalidates native translations intersecting a byte range.
    pub fn flush_tlb_range(start: crate::VirtAddr, size: usize) {
        flush_tlb_range(start, size);
    }
    /// Returns the current CPU's native hardware address-space tag capacity.
    pub fn address_space_tag_capacity() -> u32 {
        address_space_tag_capacity()
    }
}

#[cfg(target_arch = "riscv64")]
pub use crate::arch::current::mmu::{Satp, SatpMode, install_page_table, read_satp};
