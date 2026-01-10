use byte_unit::{Byte, UnitType};
use kernutil::StaticCell;
pub use kernutil::memory::{MemoryDescriptor, MemoryType, PageTableInfo};
use num_align::NumAlign;
use ranges_ext::{RangeError, RangeExtBaseOps, RangeVecOps};

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
static mut KIMAGE_START: usize = 0;
static mut KIMAGE_END: usize = 0;

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

/// RAM 物理地址应当转换为的内核虚拟地址
pub fn __va(paddr: usize) -> *mut u8 {
    crate::arch::Arch::_va(paddr)
}

/// IO 物理地址应当转换为的内核虚拟地址
pub fn __io(paddr: usize) -> *mut u8 {
    crate::arch::Arch::_io(paddr)
}

// /// 内核虚拟地址转换为 RAM 物理地址
// pub(crate) fn __pa(vaddr: *const u8) -> usize {

// }

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

/// 物理RAM实际转换为的内核虚拟地址
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
    // paddr as *mut u8
}

pub fn virt_to_phys(vaddr: *const u8) -> usize {
    crate::arch::Arch::virt_to_phys(vaddr)
}

pub(crate) fn _fixmap_io(paddr: usize) -> *mut u8 {
    if mmu::is_mmu_enabled() {
        __io(paddr)
    } else {
        paddr as *mut u8
    }
}

pub(crate) fn early_init(kernel_end_phys: usize) {
    static mut INITIALIZED: bool = false;
    if unsafe { INITIALIZED } {
        return;
    }

    ram::init(kernel_end_phys);
    crate::fdt::save_fdt();
    unsafe {
        INITIALIZED = true;
    }
}

pub(crate) fn init_after_mmu() -> Option<()> {
    super::fdt::init_memory_map();
    Some(())
}

pub(crate) fn set_kernel_range(start: usize, end: usize) {
    unsafe {
        KIMAGE_START = start.align_down(page_size());
        KIMAGE_END = end.align_up(page_size());
    }
}

/// Get the physical range of the kernel image
pub(crate) fn kimage_range() -> core::ops::Range<usize> {
    unsafe { KIMAGE_START..KIMAGE_END }
}

pub fn page_size() -> usize {
    unsafe extern "C" {
        static PAGE_SIZE: usize;
    }
    core::ptr::addr_of!(PAGE_SIZE) as usize
}

fn ram_used_range() -> core::ops::Range<usize> {
    let kernel = kimage_range();
    let start = kernel.end;
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
    // let temp = unsafe {
    //     let start = phys_to_virt(Ram {}.current().align_up(page_size()) as usize);
    //     core::slice::from_raw_parts_mut(start, size_of::<MemoryMap>())
    // };

    unsafe {
        // let temp_ptr = MEMORY_MAP_TEMP.as_slice().as_ptr();
        // let len = MEMORY_MAP_TEMP.len() * core::mem::size_of::<MemoryDescriptor>();
        // let temp = core::slice::from_raw_parts_mut(temp_ptr as *mut u8, len);

        // MEMORY_MAP.update(|mem| mem.merge_add(desc, temp))

        let mut temp = MemoryMap::new();
        MEMORY_MAP.update(|mem| mem.merge_add_with_temp(desc, &mut temp))
    }
}
