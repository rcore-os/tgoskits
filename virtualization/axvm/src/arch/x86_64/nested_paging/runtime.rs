//! Runtime dispatch and shared entry translation for x86 nested page tables.

use ax_memory_addr::{PhysAddr, VirtAddr};
use axaddrspace::{AddrSpaceResult, NestedPageTableOps, PageSize};
use axvm_types::{GuestPhysAddr, MappingFlags};
use page_table_generic as ptg;

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
        match crate::arch::x86_64::policy::selected_nested_paging_format().map_err(|_| {
            crate::ax_err_type!(BadState, "x86 virtualization backend is not selected")
        })? {
            crate::arch::x86_64::policy::X86NestedPagingFormat::Ept => {
                EptNestedPageTable::new(level).map(|table| Self {
                    inner: NestedPageTableInner::Ept(table),
                })
            }
            crate::arch::x86_64::policy::X86NestedPagingFormat::Npt => {
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

    fn map_linear(
        &mut self,
        vaddr: GuestPhysAddr,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
        allow_huge: bool,
    ) -> AddrSpaceResult {
        Ok(match &mut self.inner {
            NestedPageTableInner::Ept(table) => {
                table.map_linear(vaddr, paddr, size, flags, allow_huge)
            }
            NestedPageTableInner::Npt(table) => {
                table.map_linear(vaddr, paddr, size, flags, allow_huge)
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
        Ok(match &self.inner {
            NestedPageTableInner::Ept(table) => table.query(vaddr),
            NestedPageTableInner::Npt(table) => table.query(vaddr),
        }?)
    }
}

/// The VM owner closes guest admission and waits for hardware exit before
/// mutating these tables. Every subsequent Vcpu::run invalidates its local
/// EPT/NPT context; host INVLPG/CR3 cannot invalidate either nested format.
/// Initial construction and destruction occur while this VM cannot run.
pub(super) fn flush_nested_page_table(_vaddr: Option<ptg::VirtAddr>) {}

/// Retires cached translations for a retired nested-paging root on this CPU.
///
/// The control owner unloads every vCPU and drives this on each physical CPU
/// that may have cached a translation under `old_root`, so a recycled root can
/// never be served from a stale cached translation.
///
/// Intel retirement is immediate: `INVEPT` with the retired EPTP invalidates
/// exactly this CPU's EPT-derived translations. AMD has no host instruction
/// that invalidates only NPT translations, so its retirement is deferred to a
/// guaranteed flush on the next guest entry; see the NPT arm below.
pub(crate) fn invalidate_translations(
    old_root: axvm_types::NestedPagingConfig,
) -> crate::AxVmResult {
    match crate::arch::x86_64::policy::selected_nested_paging_format().map_err(|error| {
        crate::vcpu::map_vcpu_backend_error(
            "select x86 nested paging format",
            super::super::x86_error_to_backend(error),
        )
    })? {
        crate::arch::x86_64::policy::X86NestedPagingFormat::Ept => {
            // SAFETY: the control owner unloaded every vCPU and pinned this CPU,
            // which independently derived translations from the retired EPTP.
            let pointer = unsafe {
                ax_cpu::virtualization::EptPointer::for_current_cpu(
                    PhysAddr::from_usize(old_root.root_paddr.as_usize()),
                    false,
                )
            }
            .map_err(|error| crate::AxVmError::vcpu("encode retired x86 EPT root", error))?;
            // SAFETY: the retired EPTP stays a valid operand for the duration of
            // this instruction and the CPU owns the VMX-enabled interval.
            unsafe { pointer.invalidate() }.map_err(|error| {
                crate::AxVmError::vcpu("invalidate retired x86 EPT translations", error)
            })
        }
        crate::arch::x86_64::policy::X86NestedPagingFormat::Npt => {
            // AMD provides no host-side instruction that invalidates only NPT
            // (nested) translations. `INVLPGA` covers guest-virtual mappings, and
            // a host `INVLPG`/`CR3` reload flushes only the host page tables, not
            // the nested TLB. The architectural mechanism is the guest VMCB
            // `TLB_CONTROL` field, which the CPU consumes on the next `VMRUN`.
            //
            // Retirement is therefore deferred to that entry and is *enforced*,
            // not merely assumed, by axcpu's SVM entry path: `Vcpu::run`
            // (`components/axcpu/src/arch/x86_64/virtualization/vcpu.rs`)
            // unconditionally writes `VmcbTlbControl::FlushAll` and clears
            // `clean_bits` before *every* `VMRUN`. `FlushAll` is AMD's
            // `TLB_CONTROL_FLUSH_ALL_ASID`: it invalidates the entire local TLB,
            // including every ASID-tagged entry and every nested (NPT) entry
            // derived from the retired root. Every physical CPU that can run this
            // VM performs that flush before its first guest entry after this
            // rendezvous, so no CPU can keep serving a recycled root from a stale
            // translation.
            //
            // This mirrors Linux KVM, which expresses every SVM TLB flush through
            // `vmcb->control.tlb_ctl` (`svm_flush_tlb_*` in
            // `arch/x86/kvm/svm/svm.c`) because, as KVM notes, "SVM doesn't
            // provide a way to flush only NPT TLB entries". A host
            // `CR3`/`INVLPG` flush here would be a no-op for guest translations
            // and is deliberately not used.
            Ok(())
        }
    }
}
