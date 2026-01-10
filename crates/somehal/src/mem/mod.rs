use byte_unit::{Byte, UnitType};
use kernutil::StaticCell;
pub use kernutil::memory::{MemoryDescriptor, MemoryType, PageTableInfo};
use num_align::NumAlign;
use ranges_ext::{RangeError, RangeVecOps};

pub mod mmu;
pub(crate) mod ram;
pub(crate) mod region;

use crate::{ArchTrait, mem::ram::Ram};

pub use page_table_generic::*;

pub const KB: usize = 1024;
pub const MB: usize = 1024 * KB;
pub const GB: usize = 1024 * MB;

static mut VM_LOAD_OFFSET: isize = 0;
static MEMORY_MAP: StaticCell<MemoryMap> = StaticCell::new(MemoryMap::new());

// pub type PageTable<A> = crate::arch::PT<A>;
pub type MemoryMap = heapless::Vec<MemoryDescriptor, 128>;

/// 运行地址 - 链接地址
pub(crate) fn set_vm_load_offset(offset: isize) {
    unsafe {
        VM_LOAD_OFFSET = offset;
    }
}

/// Get the offset between virtual address and physical address of the loaded kernel image
pub fn vm_load_offset() -> isize {
    unsafe { VM_LOAD_OFFSET }
}

pub fn page_offset() -> usize {
    crate::arch::Arch::PAGE_OFFSET
}

/// RAM 物理地址转换为内核虚拟地址
pub(crate) fn __va(paddr: usize) -> *mut u8 {
    (paddr + crate::arch::Arch::PAGE_OFFSET) as *mut u8
}

/// 内核虚拟地址转换为 RAM 物理地址
pub(crate) fn __pa(vaddr: *const u8) -> usize {
    vaddr as usize - crate::arch::Arch::PAGE_OFFSET
}

/// kernel image 物理地址转换为内核虚拟地址
pub(crate) fn __kimage_va(paddr: usize) -> *mut u8 {
    (paddr as isize - vm_load_offset()) as usize as *mut u8
}

pub fn memory_map() -> &'static [MemoryDescriptor] {
    MEMORY_MAP.as_slice()
}

pub fn enable_paging() {
    crate::arch::Arch::enable_paging();
}

pub fn phys_to_virt(paddr: usize) -> *mut u8 {
    if mmu::is_mmu_enabled() {
        if kimage_range().contains(&paddr) {
            __kimage_va(paddr)
        } else {
            __va(paddr)
        }
    } else {
        paddr as *mut u8
    }
}

pub fn virt_to_phys(vaddr: *const u8) -> usize {
    crate::arch::Arch::virt_to_phys(vaddr)
}

pub(crate) fn _fixmap_io(paddr: usize) -> *mut u8 {
    if mmu::is_mmu_enabled() {
        __va(paddr)
    } else {
        paddr as *mut u8
    }
}

pub(crate) fn early_init() {
    static mut INITIALIZED: bool = false;
    if unsafe { INITIALIZED } {
        return;
    }

    ram::init();
    crate::fdt::save_fdt();
    unsafe {
        INITIALIZED = true;
    }
}

pub(crate) fn init_after_mmu() -> Option<()> {
    unsafe {
        MEMORY_MAP.update(|map| map.clear());
        super::fdt::init_memory_map();
    }
    Some(())
}

/// Get the physical range of the kernel image
pub(crate) fn kimage_range() -> core::ops::Range<usize> {
    let kernel = crate::arch::Arch::kernel_code().as_ptr_range();
    let start = virt_to_phys(kernel.start);
    let end = virt_to_phys(kernel.end);
    // let end = ram::current() as usize;
    // start..end.align_up(2 * MB)
    start..end.align_up(page_size())
}

pub fn page_size() -> usize {
    unsafe extern "C" {
        static PAGE_SIZE: usize;
    }
    core::ptr::addr_of!(PAGE_SIZE) as usize
}

fn ram_used_range() -> core::ops::Range<usize> {
    let kernel = crate::arch::Arch::kernel_code().as_ptr_range();
    let start = virt_to_phys(kernel.end);
    let end = ram::current() as usize;
    start..end.align_up(page_size())
}

pub(crate) fn memory_map_setup() {
    let kernel_range = kimage_range();
    let desc = MemoryDescriptor::new_with_range("Kernel", kernel_range, MemoryType::KImage);

    add_memory_descriptor(desc).unwrap();

    let ram_range = ram_used_range();
    let desc = MemoryDescriptor::new_with_range("Some Rsv", ram_range, MemoryType::Reserved);
    add_memory_descriptor(desc).unwrap();

    if let Some(desc) = crate::console::debug_to_memory_desc() {
        add_memory_descriptor(desc).unwrap();
    }
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
    let temp = unsafe {
        let start = phys_to_virt(Ram {}.current().align_up(page_size()) as usize);
        core::slice::from_raw_parts_mut(start, size_of::<MemoryMap>())
    };

    unsafe { MEMORY_MAP.update(|mem| mem.merge_add(desc, temp)) }
}
