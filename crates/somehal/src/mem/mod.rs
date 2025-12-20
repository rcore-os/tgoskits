use byte_unit::{Byte, UnitType};
use kernutil::StaticCell;
pub use kernutil::memory::{MemoryDescriptor, MemoryType};
use num_align::NumAlign;
use ranges_ext::RangeError;

pub(crate) mod ram;
pub(crate) mod region;

use crate::ArchTrait;

pub use crate::arch::Pte;
pub use page_table_generic::*;

pub const KB: usize = 1024;
pub const MB: usize = 1024 * KB;
pub const GB: usize = 1024 * MB;

static mut VM_LOAD_OFFSET: isize = 0;
static mut MMU_ENABLED: bool = false;
static MEMORY_MAP: StaticCell<MemoryMap> = StaticCell::new(MemoryMap::new());

pub type PageTable<A> = crate::arch::PT<A>;
pub type MemoryMap = ranges_ext::RangeSet<MemoryDescriptor>;

#[derive(Debug, Clone, Copy)]
pub struct PageTableInfo {
    pub asid: usize,
    pub addr: usize,
}

pub(crate) fn set_vm_load_offset(offset: isize) {
    unsafe {
        VM_LOAD_OFFSET = offset;
    }
}

pub(crate) fn vm_load_offset() -> isize {
    unsafe { VM_LOAD_OFFSET }
}

pub fn memory_map() -> &'static [MemoryDescriptor] {
    MEMORY_MAP.as_slice()
}

pub(crate) fn set_mmu_enabled() {
    unsafe {
        MMU_ENABLED = true;
    }
}

pub(crate) fn is_mmu_enabled() -> bool {
    unsafe { MMU_ENABLED }
}

pub fn enable_paging() {
    crate::arch::Arch::enable_paging();
}

pub fn phys_to_virt(paddr: usize) -> *mut u8 {
    if is_mmu_enabled() {
        crate::arch::Arch::_va(paddr)
    } else {
        paddr as *mut u8
    }
}

pub fn virt_to_phys(vaddr: *const u8) -> usize {
    if is_mmu_enabled() {
        crate::arch::Arch::_pa(vaddr)
    } else {
        vaddr as usize
    }
}

pub fn ioremap(paddr: usize, size: usize) -> *mut u8 {
    let end = paddr + size;
    let paddr = paddr.align_down(page_size());
    let size = end.align_up(page_size()) - paddr;
    crate::arch::Arch::ioremap(paddr, size)
}

pub(crate) fn _fixmap_io(name: &'static str, paddr: usize, size: usize) -> *mut u8 {
    add_memory_descriptor(MemoryDescriptor::new_aligned(
        name,
        paddr,
        size,
        MemoryType::Mmio,
        page_size(),
    ))
    .unwrap();

    if is_mmu_enabled() {
        crate::arch::Arch::_io(paddr)
    } else {
        paddr as *mut u8
    }
}

pub(crate) fn early_init() {
    ram::init();
    crate::fdt::save_fdt();
}

pub(crate) fn kernel_range() -> core::ops::Range<usize> {
    let kernel = crate::arch::Arch::kernel_code().as_ptr_range();
    let start = virt_to_phys(kernel.start);
    let end = virt_to_phys(kernel.end);
    start..end
}

pub fn page_size() -> usize {
    unsafe extern "C" {
        static PAGE_SIZE: usize;
    }
    core::ptr::addr_of!(PAGE_SIZE) as usize
}

pub fn new_page_table<A: FrameAllocator>(allocator: A) -> PageTable<A> {
    crate::arch::Arch::create_page_table(allocator)
}

pub(crate) fn memory_map_setup() {
    let kernel_range = kernel_range();
    let desc = MemoryDescriptor::new_with_range("Kernel", kernel_range, MemoryType::Reserved);

    add_memory_descriptor(desc).unwrap();

    let desc = ram::to_rsvd_memory_descriptor();
    add_memory_descriptor(desc).unwrap();
}

pub fn print_memory_map() {
    println!("Memory Map:");
    for desc in memory_map().iter() {
        let fmt = Byte::from(desc.size_in_bytes).get_appropriate_unit(UnitType::Binary);
        println!(
            "  {:<20} {:>#016x} - {:>#016x} ({:#.2})",
            desc.name,
            desc.physical_start,
            desc.physical_start + desc.size_in_bytes,
            fmt
        );
    }
}

pub(crate) fn add_memory_descriptor(
    desc: MemoryDescriptor,
) -> Result<(), RangeError<MemoryDescriptor>> {
    unsafe { MEMORY_MAP.update(|mem| mem.add(desc)) }
}

pub(crate) fn add_memory_descriptors(
    descs: impl Iterator<Item = MemoryDescriptor>,
) -> Result<(), RangeError<MemoryDescriptor>> {
    for desc in descs {
        add_memory_descriptor(desc)?;
    }
    Ok(())
}
