//! Runtime dispatch and shared entry translation for x86 nested page tables.

use ax_memory_addr::{PhysAddr, VirtAddr};
use ax_page_table::stage2 as ptg;
use axaddrspace::{AddrSpaceResult, NestedPageTableOps, PageSize};
use axvm_types::{GuestPhysAddr, MappingFlags};

use super::{ept::EptPageTableMetadata, npt::NptPageTableMetadata};

// EPT and NPT share the page-table walk geometry, but their entry encodings and
// permission semantics differ. Keeping distinct aliases prevents selecting an
// AMD encoding for an Intel VM, or vice versa.
type EptNestedPageTable<H> =
    crate::npt::LeveledPageTable<EptPageTableMetadata, EptPageTableMetadata, H, false>;
type NptNestedPageTable<H> =
    crate::npt::LeveledPageTable<NptPageTableMetadata, NptPageTableMetadata, H, false>;

/// Runtime-selected x86 nested page table.
pub(crate) struct NestedPageTable<H: crate::host::PagingHandler + 'static> {
    inner: NestedPageTableInner<H>,
}

/// The concrete page-table encoding fixed when the x86 runtime is initialized.
enum NestedPageTableInner<H: crate::host::PagingHandler + 'static> {
    Ept(EptNestedPageTable<H>),
    Npt(NptNestedPageTable<H>),
}

impl<H: crate::host::PagingHandler + 'static> NestedPageTable<H> {
    /// Create a table whose entry encoding matches the already selected CPU backend.
    ///
    /// The runtime chooses once before VM resources are created, so a VM cannot
    /// accidentally mix EPT and NPT entries while its vCPUs use one backend.
    pub(crate) fn new(level: usize) -> crate::AxVmResult<Self> {
        match x86_vcpu::selected_nested_paging_format().map_err(|_| {
            crate::ax_err_type!(BadState, "x86 virtualization backend is not selected")
        })? {
            x86_vcpu::X86NestedPagingFormat::Ept => {
                EptNestedPageTable::new(level).map(|table| Self {
                    inner: NestedPageTableInner::Ept(table),
                })
            }
            x86_vcpu::X86NestedPagingFormat::Npt => {
                NptNestedPageTable::new(level).map(|table| Self {
                    inner: NestedPageTableInner::Npt(table),
                })
            }
        }
    }
}

impl<H: crate::host::PagingHandler + 'static> NestedPageTableOps for NestedPageTable<H> {
    fn root_paddr(&self) -> PhysAddr {
        match &self.inner {
            NestedPageTableInner::Ept(table) => table.root_paddr(),
            NestedPageTableInner::Npt(table) => table.root_paddr(),
        }
    }

    fn levels(&self) -> usize {
        match &self.inner {
            NestedPageTableInner::Ept(table) => table.levels(),
            NestedPageTableInner::Npt(table) => table.levels(),
        }
    }

    fn alloc_frame(&self) -> Option<PhysAddr> {
        match &self.inner {
            NestedPageTableInner::Ept(table) => table.alloc_frame(),
            NestedPageTableInner::Npt(table) => table.alloc_frame(),
        }
    }

    fn dealloc_frame(&self, paddr: PhysAddr) {
        match &self.inner {
            NestedPageTableInner::Ept(table) => table.dealloc_frame(paddr),
            NestedPageTableInner::Npt(table) => table.dealloc_frame(paddr),
        }
    }

    fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr {
        match &self.inner {
            NestedPageTableInner::Ept(table) => table.phys_to_virt(paddr),
            NestedPageTableInner::Npt(table) => table.phys_to_virt(paddr),
        }
    }

    fn map(
        &mut self,
        vaddr: GuestPhysAddr,
        paddr: PhysAddr,
        size: PageSize,
        flags: MappingFlags,
    ) -> AddrSpaceResult {
        Ok(match &mut self.inner {
            NestedPageTableInner::Ept(table) => table.map(vaddr, paddr, size, flags),
            NestedPageTableInner::Npt(table) => table.map(vaddr, paddr, size, flags),
        }?)
    }

    fn unmap(
        &mut self,
        vaddr: GuestPhysAddr,
    ) -> AddrSpaceResult<(PhysAddr, MappingFlags, PageSize)> {
        Ok(match &mut self.inner {
            NestedPageTableInner::Ept(table) => table.unmap(vaddr),
            NestedPageTableInner::Npt(table) => table.unmap(vaddr),
        }?)
    }

    fn map_region(
        &mut self,
        vaddr: GuestPhysAddr,
        get_paddr: impl Fn(GuestPhysAddr) -> PhysAddr,
        size: usize,
        flags: MappingFlags,
        allow_huge: bool,
    ) -> AddrSpaceResult {
        Ok(match &mut self.inner {
            NestedPageTableInner::Ept(table) => {
                table.map_region(vaddr, &get_paddr, size, flags, allow_huge)
            }
            NestedPageTableInner::Npt(table) => {
                table.map_region(vaddr, &get_paddr, size, flags, allow_huge)
            }
        }?)
    }

    fn unmap_region(&mut self, start: GuestPhysAddr, size: usize) -> AddrSpaceResult {
        Ok(match &mut self.inner {
            NestedPageTableInner::Ept(table) => table.unmap_region(start, size),
            NestedPageTableInner::Npt(table) => table.unmap_region(start, size),
        }?)
    }

    fn remap(&mut self, start: GuestPhysAddr, paddr: PhysAddr, flags: MappingFlags) -> bool {
        match &mut self.inner {
            NestedPageTableInner::Ept(table) => table.remap(start, paddr, flags),
            NestedPageTableInner::Npt(table) => table.remap(start, paddr, flags),
        }
    }

    fn protect_region(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
        new_flags: MappingFlags,
    ) -> bool {
        match &mut self.inner {
            NestedPageTableInner::Ept(table) => table.protect_region(start, size, new_flags),
            NestedPageTableInner::Npt(table) => table.protect_region(start, size, new_flags),
        }
    }

    fn query(&self, vaddr: GuestPhysAddr) -> AddrSpaceResult<(PhysAddr, MappingFlags, PageSize)> {
        match &self.inner {
            NestedPageTableInner::Ept(table) => table.query(vaddr),
            NestedPageTableInner::Npt(table) => table.query(vaddr),
        }
    }
}

/// Convert generic page-table permissions to the subset shared by EPT and NPT.
///
/// The architecture-specific entry modules retain ownership of memory-type and
/// reserved-bit encoding; this conversion deliberately carries only the common
/// access semantics.
pub(super) fn config_to_flags(config: ptg::PteConfig) -> MappingFlags {
    let mut flags = MappingFlags::empty();
    if config.read {
        flags |= MappingFlags::READ;
    }
    if config.writable {
        flags |= MappingFlags::WRITE;
    }
    if config.executable {
        flags |= MappingFlags::EXECUTE;
    }
    if config.lower {
        flags |= MappingFlags::USER;
    }
    match config.mem_attr {
        ptg::MemAttributes::Device => flags |= MappingFlags::DEVICE,
        ptg::MemAttributes::Uncached => flags |= MappingFlags::UNCACHED,
        _ => {}
    }
    flags
}

#[cfg(target_os = "none")]
/// Invalidate host translations after ax-page-table changes an entry.
///
/// The generic walker invokes this callback after installing or removing an
/// entry. It may provide one address for a targeted invalidation or no address
/// when the whole table must be invalidated.
pub(super) fn flush_nested_page_table(vaddr: Option<ptg::VirtAddr>) {
    if let Some(vaddr) = vaddr {
        // SAFETY: ax-page-table calls this after changing the current CPU's
        // translation entries; `vaddr` is a virtual address belonging to that table.
        unsafe { x86::tlb::flush(vaddr.as_usize()) }
    } else {
        // SAFETY: ax-page-table requests a full invalidation after changing
        // entries without a single virtual-address target.
        unsafe { x86::tlb::flush_all() }
    }
}

#[cfg(not(target_os = "none"))]
/// Host-side tests do not execute with the kernel page tables installed.
pub(super) fn flush_nested_page_table(_vaddr: Option<ptg::VirtAddr>) {}
