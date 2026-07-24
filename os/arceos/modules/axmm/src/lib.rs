//! [ArceOS](https://github.com/arceos-org/arceos) memory management module.

#![no_std]

#[macro_use]
extern crate log;
extern crate alloc;

mod aspace;
mod backend;

use ax_errno::{AxError, AxResult};
use ax_hal::{
    mem::{IomapAttrs, IomapDecision, IomapError, MemRegionFlags, phys_to_virt},
    paging::MappingFlags,
};
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange};

pub use self::{aspace::AddrSpace, backend::Backend};

static KERNEL_ASPACE: LazyInit<SpinNoIrq<AddrSpace>> = LazyInit::new();

fn reg_flag_to_map_flag(f: MemRegionFlags) -> MappingFlags {
    let mut ret = MappingFlags::empty();
    if f.contains(MemRegionFlags::READ) {
        ret |= MappingFlags::READ;
    }
    if f.contains(MemRegionFlags::WRITE) {
        ret |= MappingFlags::WRITE;
    }
    if f.contains(MemRegionFlags::EXECUTE) {
        ret |= MappingFlags::EXECUTE;
    }
    if f.contains(MemRegionFlags::DEVICE) {
        ret |= MappingFlags::DEVICE;
    }
    if f.contains(MemRegionFlags::UNCACHED) {
        ret |= MappingFlags::UNCACHED;
    }
    ret
}

#[cfg(feature = "copy")]
/// Creates a new address space for user processes.
pub fn new_user_aspace(base: VirtAddr, size: usize) -> AxResult<AddrSpace> {
    let mut aspace = AddrSpace::new_empty(base, size)?;
    if ax_hal::mem::user_aspace_needs_kernel_mappings() {
        aspace.copy_mappings_from(&kernel_aspace().lock())?;
    }
    Ok(aspace)
}

/// Creates a new address space for kernel itself.
pub fn new_kernel_aspace() -> AxResult<AddrSpace> {
    let (base, size) = ax_hal::mem::kernel_aspace();
    let mut aspace = AddrSpace::new_empty(base, size)?;
    for r in ax_hal::mem::memory_regions() {
        // mapped range should contain the whole region if it is not aligned.
        let start = r.paddr.align_down_4k();
        let end = (r.paddr + r.size).align_up_4k();
        let vaddr = phys_to_virt(start);
        let size = end - start;

        // Some platforms provide a physical direct map outside the
        // page-table-backed kernel address space. Those ranges must not be
        // inserted into this address space because their low VA bits can alias
        // real page-table mappings such as vmap.
        if aspace.contains_range(vaddr, size) {
            aspace.map_linear(vaddr, start, size, reg_flag_to_map_flag(r.flags))?;
        }
    }
    Ok(aspace)
}

/// Returns the globally unique kernel address space.
pub fn kernel_aspace() -> &'static SpinNoIrq<AddrSpace> {
    &KERNEL_ASPACE
}

/// Returns the root physical address of the kernel page table.
pub fn kernel_page_table_root() -> PhysAddr {
    KERNEL_ASPACE.lock().page_table_root()
}

/// Initializes virtual memory management.
///
/// It mainly sets up the kernel virtual memory address space and recreate a
/// fine-grained kernel page table.
pub fn init_memory_management() {
    info!("Initialize virtual memory management...");
    ax_hal::paging::validate_smp_invalidation();

    let kernel_aspace = new_kernel_aspace().expect("failed to initialize kernel address space");
    debug!("kernel address space init OK: {kernel_aspace:#x?}");
    KERNEL_ASPACE.init_once(SpinNoIrq::new(kernel_aspace));
    unsafe {
        ax_hal::asm::write_kernel_page_table(kernel_page_table_root());
        ax_hal::asm::flush_tlb(None);
    }
}

/// Initializes kernel paging for secondary CPUs.
pub fn init_memory_management_secondary() {
    unsafe {
        ax_hal::asm::write_kernel_page_table(kernel_page_table_root());
        ax_hal::asm::flush_tlb(None);
    }
}

/// Maps a physical memory region to virtual address space for device access.
pub fn iomap(addr: PhysAddr, size: usize) -> AxResult<VirtAddr> {
    if size == 0 {
        return Err(AxError::InvalidInput);
    }
    addr.as_usize()
        .checked_add(size)
        .ok_or(AxError::InvalidInput)?;
    let decision =
        ax_hal::mem::prepare_iomap(addr, size, IomapAttrs::DEVICE).map_err(
            |error| match error {
                IomapError::InvalidInput => AxError::InvalidInput,
                IomapError::Unsupported => AxError::Unsupported,
            },
        )?;
    match decision {
        IomapDecision::Mapped(vaddr) => Ok(vaddr),
        IomapDecision::UseGeneric(paddr) => iomap_generic(paddr, size),
    }
}

fn iomap_generic(addr: PhysAddr, size: usize) -> AxResult<VirtAddr> {
    let end = addr
        .as_usize()
        .checked_add(size)
        .ok_or(AxError::InvalidInput)?;
    let virt = phys_to_virt(addr);

    let virt_aligned = virt.align_down_4k();
    let addr_aligned = addr.align_down_4k();
    let end_aligned = end
        .checked_add(PAGE_SIZE_4K - 1)
        .ok_or(AxError::InvalidInput)?
        & !(PAGE_SIZE_4K - 1);
    let size_aligned = PhysAddr::from_usize(end_aligned) - addr_aligned;
    let offset = addr - addr_aligned;

    let flags = MappingFlags::DEVICE | MappingFlags::READ | MappingFlags::WRITE;
    let mut tb = kernel_aspace().lock();

    let mapped = if tb.contains_range(virt_aligned, size_aligned) {
        match tb.map_linear(virt_aligned, addr_aligned, size_aligned, flags) {
            Err(AxError::AlreadyExists) => {
                tb.map_linear_overwrite(virt_aligned, addr_aligned, size_aligned, flags)?;
            }
            Err(e) => {
                return Err(e);
            }
            Ok(_) => {}
        }
        virt_aligned
    } else {
        // On platforms where `phys_to_virt()` is a hardware direct map outside
        // the page-table-backed kernel address space, allocate a separate
        // kernel VA and map the device with PTE attributes.
        let range = VirtAddrRange::new(tb.base(), tb.end());
        let mapped = tb
            .find_free_area(tb.base(), size_aligned, range)
            .ok_or(AxError::NoMemory)?;
        tb.map_linear(mapped, addr_aligned, size_aligned, flags)?;
        mapped
    };

    Ok(mapped + offset)
}
