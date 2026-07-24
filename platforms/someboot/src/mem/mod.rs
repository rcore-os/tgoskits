use core::ops::Range;

use byte_unit::{Byte, UnitType};
use kernutil::StaticCell;
pub use kernutil::memory::{
    MemoryDescriptor, MemoryMapExt, MemoryRangeError, MemoryType, PageTableInfo,
};
use num_align::NumAlign;

pub mod mmu;
pub(crate) mod ram;
pub(crate) mod region;

pub use page_table_generic::*;

use crate::{ArchTrait, DCacheOp, arch::Arch, smp::cpu_area_region};

pub const KB: usize = 1024;
pub const MB: usize = 1024 * KB;
pub const GB: usize = 1024 * MB;
pub const KIMAGE_MAP_ALIGN: usize = 2 * MB;

static mut VM_LOAD_OFFSET: isize = 0;
static MEMORY_MAP: StaticCell<MemoryMap> = StaticCell::new(MemoryMap::new());

/// Load address of the kernel start
static mut KIMAGE_START: Option<PhysAddr> = None;
/// Load address of the kernel end
static mut KIMAGE_END: PhysAddr = PhysAddr::from_usize(0);

const MEMORY_MAP_CAPACITY: usize = 512;

pub type MemoryMap = heapless::Vec<MemoryDescriptor, MEMORY_MAP_CAPACITY>;

pub(crate) fn setup_entry(
    kernel_start: PhysAddr,
    kernel_end: PhysAddr,
    kernel_start_link: VirtAddr,
) {
    unsafe {
        KIMAGE_START = Some(kernel_start);
        KIMAGE_END = kernel_end.as_usize().align_up(KIMAGE_MAP_ALIGN).into();

        VM_LOAD_OFFSET = kernel_start.as_usize() as isize - kernel_start_link.as_usize() as isize;
    }
}

pub fn stack_size() -> usize {
    unsafe extern "C" {
        fn STACK_SIZE();
    }
    STACK_SIZE as *const () as usize
}

/// Get the offset between virtual address and physical address of the loaded kernel image
pub fn vm_load_offset() -> isize {
    unsafe { VM_LOAD_OFFSET }
}

/// Converts a RAM physical address to its kernel virtual address.
pub fn __va(paddr: usize) -> *mut u8 {
    crate::arch::Arch::_va(paddr)
}

/// Converts an I/O physical address to its kernel virtual address.
pub fn __io(paddr: usize) -> *mut u8 {
    crate::arch::Arch::_io(paddr)
}

pub fn cpu_area_phys_to_virt(paddr: usize) -> *mut u8 {
    crate::arch::Arch::cpu_area_phys_to_virt(paddr)
}

/// Converts a kernel-image physical address to its linked virtual address.
pub(crate) fn __kimage_va(paddr: usize) -> *mut u8 {
    (paddr as isize - vm_load_offset()) as usize as *mut u8
}

pub(crate) fn __kimage_va_to_pa(vaddr: *const u8) -> usize {
    (vaddr as usize as isize + vm_load_offset()) as usize
}

pub fn memory_map() -> &'static [MemoryDescriptor] {
    MEMORY_MAP.as_slice()
}

pub fn dcache_range(op: DCacheOp, addr: *const u8, size: usize) {
    Arch::dcache_range(op, addr as _, size);
}

pub fn dma_coherent_before_make_uncached(addr: *const u8, size: usize) {
    Arch::dma_coherent_before_make_uncached(addr as _, size);
}

pub fn dma_coherent_before_restore_cached(addr: *const u8, size: usize) {
    Arch::dma_coherent_before_restore_cached(addr as _, size);
}

pub fn dma_coherent_after_mapping_update() {
    Arch::dma_coherent_after_mapping_update();
}

#[cfg(any(test, all(target_arch = "riscv64", feature = "thead-mae")))]
pub(crate) fn cache_line_range(
    addr: usize,
    size: usize,
    line_size: usize,
) -> Option<(usize, usize)> {
    if size == 0 || line_size == 0 || !line_size.is_power_of_two() {
        return None;
    }
    let end = addr.checked_add(size)?;
    Some((addr & !(line_size - 1), end))
}

/// Converts a physical RAM address according to the active boot mapping.
pub fn phys_to_virt(paddr: usize) -> *mut u8 {
    if mmu::is_kernel_relocated() {
        if kimage_range().contains(&paddr) {
            __kimage_va(paddr)
        } else if cpu_area_region().contains(&paddr) {
            cpu_area_phys_to_virt(paddr)
        } else {
            __va(paddr)
        }
    } else if cfg!(target_arch = "loongarch64") {
        __va(paddr)
    } else {
        paddr as *mut u8
    }
}

pub fn virt_to_phys(vaddr: *const u8) -> usize {
    crate::arch::Arch::virt_to_phys(vaddr)
}

pub(crate) fn _fixmap_io(paddr: usize) -> *mut u8 {
    if mmu::is_kernel_relocated() || cfg!(target_arch = "loongarch64") {
        __io(paddr)
    } else {
        paddr as *mut u8
    }
}

pub(crate) fn early_init() {
    crate::fdt::init_memory_map();

    let kernel_range = kimage_range();
    add_memory_descriptor(MemoryDescriptor {
        physical_start: kernel_range.start,
        size_in_bytes: kernel_range.end - kernel_range.start,
        memory_type: MemoryType::KImage,
    })
    .unwrap_or_else(|err| {
        panic!("failed to add KImage memory descriptor {kernel_range:#x?}: {err:?}")
    });
    reserve_arch_early_ranges();

    unsafe { MEMORY_MAP.update(|m| m.sort_by_key(|a| a.physical_start)) };

    print_memory_map();

    let minimum_size = crate::smp::layout::planned_cpu_area_size()
        .checked_add(crate::fdt::copy_size())
        .and_then(|size| size.checked_add(KIMAGE_MAP_ALIGN))
        .expect("early RAM requirement overflowed");
    ram::init(
        select_early_ram(memory_map(), minimum_size)
            .expect("no early RAM region can hold CPU areas and boot metadata"),
    );

    crate::fdt::save_fdt();
    crate::smp::alloc_percpu();
}

fn select_early_ram(descriptors: &[MemoryDescriptor], minimum_size: usize) -> Option<Range<usize>> {
    descriptors
        .iter()
        .filter(|desc| desc.memory_type == MemoryType::Free && desc.size_in_bytes != 0)
        .filter_map(|desc| {
            let end = desc
                .physical_start
                .checked_add(desc.size_in_bytes)?
                .min(Arch::EARLY_RAM_END_EXCLUSIVE);
            let size = end.checked_sub(desc.physical_start)?;
            (size >= minimum_size.max(1)).then_some(desc.physical_start..end)
        })
        .min_by_key(|range| range.start)
}

fn reserve_arch_early_ranges() {
    if let Some(range) = Arch::EARLY_RESERVED_RANGE {
        let desc = MemoryDescriptor::new_aligned(
            range.start,
            range.end - range.start,
            MemoryType::Reserved,
            page_size(),
        )
        .expect("architecture early-memory range must be valid and aligned");
        match add_memory_descriptor(desc) {
            Ok(()) => {}
            Err(MemoryRangeError::Conflict { existing, .. })
                if existing.memory_type != MemoryType::Free =>
            {
                // Already reserved by firmware map; keep it as-is.
            }
            Err(err) => panic!("failed to reserve architecture early-memory range: {err:?}"),
        }
    }
}

/// Get the physical range of the kernel image
pub(crate) fn kimage_range() -> core::ops::Range<usize> {
    unsafe {
        let Some(start) = KIMAGE_START else {
            panic!("Kernel image start is not set");
        };
        let end = KIMAGE_END;
        start.as_usize()..end.as_usize()
    }
}

pub fn page_size() -> usize {
    unsafe extern "C" {
        static PAGE_SIZE: usize;
    }
    core::ptr::addr_of!(PAGE_SIZE) as usize
}

pub(crate) fn memory_map_setup() {
    // let kernel_range = kimage_range();
    // let desc = MemoryDescriptor::new_with_range(kernel_range, MemoryType::KImage);

    // add_memory_descriptor(desc).unwrap();

    let ram_range = ram::used_range();
    if !ram_range.is_empty() {
        let desc = MemoryDescriptor::new_with_range(ram_range, MemoryType::Reserved);
        add_memory_descriptor(desc).unwrap();
    }
    if let Some(desc) = crate::console::debug_to_memory_desc() {
        add_memory_descriptor(desc).unwrap();
    }
    ram::freeze();
}

pub fn print_memory_map() {
    println!("Memory Map:");
    unsafe { MEMORY_MAP.update(|m| m.sort_by_key(|m| m.physical_start)) };

    for desc in memory_map().iter() {
        let fmt = Byte::from(desc.size_in_bytes).get_appropriate_unit(UnitType::Binary);
        println!(
            "  {} {:>#016x} - {:>#016x} ({:#.2})",
            desc.memory_type,
            desc.physical_start,
            desc.physical_start + desc.size_in_bytes,
            fmt
        );
    }
}

pub(crate) fn add_memory_descriptor(desc: MemoryDescriptor) -> Result<(), MemoryRangeError> {
    unsafe { MEMORY_MAP.update(|mem| mem.merge_add(desc)) }
}

pub fn kernel_space() -> Range<usize> {
    Arch::kernel_space()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_line_range_covers_unaligned_buffer() {
        assert_eq!(cache_line_range(0x1003, 1, 64), Some((0x1000, 0x1004)));
        assert_eq!(cache_line_range(0x103f, 2, 64), Some((0x1000, 0x1041)));
    }

    #[test]
    fn cache_line_range_skips_empty_invalid_line_and_overflow() {
        assert_eq!(cache_line_range(0x1000, 0, 64), None);
        assert_eq!(cache_line_range(0x1000, 1, 0), None);
        assert_eq!(cache_line_range(0x1000, 1, 63), None);
        assert_eq!(cache_line_range(usize::MAX, 2, 64), None);
    }

    #[test]
    fn early_ram_prefers_the_lowest_usable_firmware_region() {
        let descriptors = [
            MemoryDescriptor {
                physical_start: 0x1000_0000,
                size_in_bytes: 16 * MB,
                memory_type: MemoryType::Free,
            },
            MemoryDescriptor {
                physical_start: 0x2000_0000,
                size_in_bytes: 128 * MB,
                memory_type: MemoryType::Free,
            },
            MemoryDescriptor {
                physical_start: 0x3000_0000,
                size_in_bytes: 256 * MB,
                memory_type: MemoryType::Reserved,
            },
        ];

        assert_eq!(
            select_early_ram(&descriptors, 3 * MB),
            Some(0x1000_0000..0x1100_0000)
        );
    }

    #[test]
    fn early_ram_accepts_small_valid_region_and_rejects_overflow() {
        let descriptors = [
            MemoryDescriptor {
                physical_start: 0x1000,
                size_in_bytes: 2 * MB,
                memory_type: MemoryType::Free,
            },
            MemoryDescriptor {
                physical_start: usize::MAX - 0x1000,
                size_in_bytes: 16 * MB,
                memory_type: MemoryType::Free,
            },
        ];

        assert_eq!(
            select_early_ram(&descriptors, 2 * MB),
            Some(0x1000..0x20_1000)
        );
    }

    #[test]
    fn early_ram_skips_a_low_region_smaller_than_the_boot_requirement() {
        let descriptors = [
            MemoryDescriptor {
                physical_start: 0x4000_0000,
                size_in_bytes: 2 * MB,
                memory_type: MemoryType::Free,
            },
            MemoryDescriptor {
                physical_start: 0x4060_0000,
                size_in_bytes: 64 * MB,
                memory_type: MemoryType::Free,
            },
        ];

        assert_eq!(
            select_early_ram(&descriptors, 3 * MB),
            Some(0x4060_0000..0x4460_0000)
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_early_ram_keeps_the_boot_page_table_root_below_4g() {
        let descriptors = [
            MemoryDescriptor {
                physical_start: 0x20_0000,
                size_in_bytes: 0x42d7_b000,
                memory_type: MemoryType::Free,
            },
            MemoryDescriptor {
                physical_start: 0x1_0000_0000,
                size_in_bytes: 0x7_8000_0000,
                memory_type: MemoryType::Free,
            },
        ];

        assert_eq!(
            select_early_ram(&descriptors, 3 * MB),
            Some(0x20_0000..0x42f7_b000)
        );
    }
}
