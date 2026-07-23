//! RISC-V specific page table structures.

use ax_memory_addr::VirtAddr;

use crate::{
    entry::riscv::Rv64PTE,
    stage1::{PageTable64, PageTable64Cursor, PagingMetaData, TlbInvalidator, TlbScope},
};

/// A virtual address that can be used in RISC-V Sv39 and Sv48 page tables.
pub trait SvVirtAddr: ax_memory_addr::MemoryAddr + Send + Sync {
    /// Flush the TLB.
    fn flush_tlb(vaddr: Option<Self>);
}

/// RISC-V local `sfence.vma`; SMP callers must provide remote fences/IPIs.
pub struct RiscvTlbInvalidator<VA>(core::marker::PhantomData<VA>);

impl<VA: SvVirtAddr> TlbInvalidator<VA> for RiscvTlbInvalidator<VA> {
    const SCOPE: TlbScope = TlbScope::Local;

    fn invalidate(vaddr: Option<VA>) {
        VA::flush_tlb(vaddr);
    }
}

impl SvVirtAddr for VirtAddr {
    #[inline]
    fn flush_tlb(vaddr: Option<Self>) {
        if let Some(vaddr) = vaddr {
            riscv::asm::sfence_vma(0, vaddr.as_usize())
        } else {
            riscv::asm::sfence_vma_all();
        }
    }
}

/// Metadata of RISC-V Sv39 page tables.
pub struct Sv39MetaData<VA: SvVirtAddr, Tlb = RiscvTlbInvalidator<VA>> {
    _virt_addr: core::marker::PhantomData<(VA, Tlb)>,
}

/// Metadata of RISC-V Sv48 page tables.
pub struct Sv48MetaData<VA: SvVirtAddr, Tlb = RiscvTlbInvalidator<VA>> {
    _virt_addr: core::marker::PhantomData<(VA, Tlb)>,
}

impl<VA: SvVirtAddr, Tlb: TlbInvalidator<VA>> PagingMetaData for Sv39MetaData<VA, Tlb> {
    const LEVELS: usize = 3;
    const PA_MAX_BITS: usize = 56;
    const VA_MAX_BITS: usize = 39;

    type VirtAddr = VA;
    type Tlb = Tlb;
}

impl<VA: SvVirtAddr, Tlb: TlbInvalidator<VA>> PagingMetaData for Sv48MetaData<VA, Tlb> {
    const LEVELS: usize = 4;
    const PA_MAX_BITS: usize = 56;
    const VA_MAX_BITS: usize = 48;

    type VirtAddr = VA;
    type Tlb = Tlb;
}

/// Sv39: Page-Based 39-bit (3 levels) Virtual-Memory System.
pub type Sv39PageTable<H, Tlb = RiscvTlbInvalidator<VirtAddr>> =
    PageTable64<Sv39MetaData<VirtAddr, Tlb>, Rv64PTE, H>;
/// Sv39 page table cursor.
pub type Sv39PageTableCursor<'a, H, Tlb = RiscvTlbInvalidator<VirtAddr>> =
    PageTable64Cursor<'a, Sv39MetaData<VirtAddr, Tlb>, Rv64PTE, H>;

/// Sv48: Page-Based 48-bit (4 levels) Virtual-Memory System.
pub type Sv48PageTable<H, Tlb = RiscvTlbInvalidator<VirtAddr>> =
    PageTable64<Sv48MetaData<VirtAddr, Tlb>, Rv64PTE, H>;
/// Sv48 page table cursor.
pub type Sv48PageTableCursor<'a, H, Tlb = RiscvTlbInvalidator<VirtAddr>> =
    PageTable64Cursor<'a, Sv48MetaData<VirtAddr, Tlb>, Rv64PTE, H>;
