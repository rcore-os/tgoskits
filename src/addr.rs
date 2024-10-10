use memory_addr::{def_usize_addr, def_usize_addr_formatter, AddrRange, PhysAddr, VirtAddr};

/// Host virtual address.
pub type HostVirtAddr = VirtAddr;
/// Host physical address.
pub type HostPhysAddr = PhysAddr;

def_usize_addr! {
    /// Guest virtual address.
    pub type GuestVirtAddr;
    /// Guest physical address.
    pub type GuestPhysAddr;
}

def_usize_addr_formatter! {
    GuestVirtAddr = "GVA:{}";
    GuestPhysAddr = "GPA:{}";
}

/// Guest virtual address range.
pub type GuestVirtAddrRange = AddrRange<GuestVirtAddr>;
/// Guest physical address range.
pub type GuestPhysAddrRange = AddrRange<GuestPhysAddr>;

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
impl page_table_multiarch::riscv::SvVirtAddr for GuestPhysAddr {
    fn flush_tlb(_vaddr: Option<Self>) {
        todo!()
    }
}
