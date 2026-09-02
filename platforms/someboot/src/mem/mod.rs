use core::ops::Range;

use byte_unit::{Byte, UnitType};
use kernutil::StaticCell;
pub use kernutil::memory::{MemoryDescriptor, MemoryType, PageTableInfo};
use num_align::NumAlign;
use ranges_ext::*;

pub mod mmu;
pub(crate) mod ram;
pub(crate) mod region;

pub use mmu::{MemAttributes, PteConfig};
pub use page_table_generic::*;

use crate::{ArchTrait, DCacheOp, arch::Arch, smp::cpu_area_region};

pub const KB: usize = 1024;
pub const MB: usize = 1024 * KB;
pub const GB: usize = 1024 * MB;
pub const KIMAGE_MAP_ALIGN: usize = 2 * MB;

/// Invalid platform virtual-address geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VirtualAddressSpaceError {
    /// One of the half-open address ranges has its end below its start.
    #[error("invalid virtual-address-space range")]
    InvalidRange,
    /// The user-capable and kernel page-table ranges overlap.
    #[error("user and kernel virtual-address-space ranges overlap")]
    OverlappingRanges,
    /// CPUCFG reports an address width unsupported by the configured walker.
    #[error("unsupported virtual-address width: VALEN={valen}")]
    UnsupportedAddressWidth {
        /// Architectural VALEN value, including the sign bit.
        valen: usize,
    },
}

/// Platform-owned page-table virtual-address layout.
///
/// Direct-map windows that bypass the page-table walker are deliberately not
/// part of `kernel`. An empty `user` range means that the current build does
/// not expose a user page table even if the architecture could support one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualAddressSpaceLayout {
    user: Range<usize>,
    kernel: Range<usize>,
}

impl VirtualAddressSpaceLayout {
    /// Validates and constructs a platform layout.
    pub fn try_new(
        user: Range<usize>,
        kernel: Range<usize>,
    ) -> Result<Self, VirtualAddressSpaceError> {
        if user.start > user.end || kernel.start > kernel.end {
            return Err(VirtualAddressSpaceError::InvalidRange);
        }
        if user.start < kernel.end && kernel.start < user.end {
            return Err(VirtualAddressSpaceError::OverlappingRanges);
        }
        Ok(Self { user, kernel })
    }

    /// Returns the lower range available to a user page table.
    pub fn user(&self) -> Range<usize> {
        self.user.clone()
    }

    /// Returns the page-table-backed kernel allocation range.
    pub fn kernel(&self) -> Range<usize> {
        self.kernel.clone()
    }
}

pub(crate) fn configured_user_space(end: usize) -> Range<usize> {
    #[cfg(uspace)]
    {
        0..end
    }
    #[cfg(not(uspace))]
    {
        let _ = end;
        0..0
    }
}

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

/// RAM 物理地址应当转换为的内核虚拟地址
pub fn __va(paddr: usize) -> *mut u8 {
    crate::arch::Arch::_va(paddr)
}

/// IO 物理地址应当转换为的内核虚拟地址
pub fn __io(paddr: usize) -> *mut u8 {
    crate::arch::Arch::_io(paddr)
}

pub fn cpu_area_phys_to_virt(paddr: usize) -> *mut u8 {
    crate::arch::Arch::cpu_area_phys_to_virt(paddr)
}

/// kernel image 物理地址转换为内核虚拟地址
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

pub fn dma_coherent_before_map_uncached(addr: *const u8, size: usize) {
    Arch::dma_coherent_before_map_uncached(addr as _, size);
}

pub fn dma_coherent_before_unmap_uncached(addr: *const u8, size: usize) {
    Arch::dma_coherent_before_unmap_uncached(addr as _, size);
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

/// 物理RAM实际转换为的内核虚拟地址
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

    let mut free_range = None;

    for desc in memory_map().iter() {
        if desc.memory_type == MemoryType::Free && desc.size_in_bytes > 8 * MB {
            free_range = Some(desc.physical_start..(desc.physical_start + desc.size_in_bytes));
            break;
        }
    }

    ram::init(free_range.expect("No free memory"));

    crate::fdt::save_fdt();
    crate::smp::alloc_percpu();
}

fn reserve_arch_early_ranges() {
    #[cfg(target_arch = "x86_64")]
    {
        // AP trampoline lives in low memory and must stay reserved.
        let tramp = crate::arch::power::AP_TRAMPOLINE_PADDR;
        let desc =
            MemoryDescriptor::new_aligned(tramp, page_size(), MemoryType::Reserved, page_size());
        match add_memory_descriptor(desc) {
            Ok(()) => {}
            Err(RangeError::Conflict { existing, .. })
                if existing.memory_type != MemoryType::Free =>
            {
                // Already reserved by firmware map; keep it as-is.
            }
            Err(err) => panic!("failed to reserve x86 AP trampoline: {err:?}"),
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

pub(crate) fn add_memory_descriptor(
    desc: MemoryDescriptor,
) -> Result<(), RangeError<MemoryDescriptor>> {
    unsafe { MEMORY_MAP.update(|mem| mem.merge_add(desc)) }
}

pub fn virtual_address_space() -> Result<VirtualAddressSpaceLayout, VirtualAddressSpaceError> {
    Arch::virtual_address_space()
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
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    fn mem_constants_and_cache_line_rules_hold_for_test() -> bool {
        // KB/MB/GB constants
        assert!(KB == 1024);
        assert!(MB == 1024 * KB);
        assert!(GB == 1024 * MB);

        // KIMAGE_MAP_ALIGN
        assert!(KIMAGE_MAP_ALIGN == 2 * MB);

        // cache_line_range: valid inputs
        let result = cache_line_range(0x1000, 64, 64).unwrap();
        assert!(result.0 == 0x1000); // aligned down
        assert!(result.1 == 0x1040); // addr + size

        // cache_line_range: zero size returns None
        assert!(cache_line_range(0x1000, 0, 64).is_none());

        // cache_line_range: zero line_size returns None
        assert!(cache_line_range(0x1000, 64, 0).is_none());

        // cache_line_range: non-power-of-2 line_size returns None
        assert!(cache_line_range(0x1000, 64, 63).is_none());

        // cache_line_range: overflow returns None
        assert!(cache_line_range(usize::MAX, 1, 64).is_none());

        true
    }

    fn mem_constants_and_types_hold_for_test() -> bool {
        // Test memory constants
        assert_eq!(KB, 1024);
        assert_eq!(MB, 1024 * 1024);
        assert_eq!(GB, 1024 * 1024 * 1024);
        assert_eq!(KIMAGE_MAP_ALIGN, 2 * MB);

        // Test MemoryMap capacity
        assert_eq!(MEMORY_MAP_CAPACITY, 512);

        true
    }

    fn mem_byte_unit_types_hold_for_test() -> bool {
        // Test byte_unit types exist
        use byte_unit::Byte;

        // Test that Byte can be created
        let _byte = Byte::from_u64(1024);

        true
    }

    #[test]
    fn mem_constants_and_cache_line_rules_hold() {
        assert!(mem_constants_and_cache_line_rules_hold_for_test());
    }

    #[test]
    fn mem_constants_and_types_hold() {
        assert!(mem_constants_and_types_hold_for_test());
    }

    #[test]
    fn mem_byte_unit_types_hold() {
        assert!(mem_byte_unit_types_hold_for_test());
    }
}
