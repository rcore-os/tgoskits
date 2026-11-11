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
/// Alignment of the boot page-table mapping, not physical KImage ownership.
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
        KIMAGE_END = kimage_reservation_end(kernel_end.as_usize()).into();

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
    #[cfg(efi)]
    crate::efi_stub::init_boot_memory_map();
    #[cfg(not(efi))]
    crate::fdt::init_memory_map();

    let kernel_range = kimage_range();
    let result = add_memory_descriptor(MemoryDescriptor {
        physical_start: kernel_range.start,
        size_in_bytes: kernel_range.end - kernel_range.start,
        memory_type: MemoryType::KImage,
    });
    if let Err(error) = result {
        #[cfg(target_arch = "loongarch64")]
        println!(
            "KImage reservation failed: range={kernel_range:#x?}, entries={}, error={error:?}",
            memory_map().len(),
        );
        panic!("failed to add KImage memory descriptor {kernel_range:#x?}: {error:?}");
    }
    reserve_arch_early_ranges();

    // This runs before the global heap exists, so sorting must not allocate.
    unsafe { MEMORY_MAP.update(|m| m.sort_unstable_by_key(|a| a.physical_start)) };

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

/// Physical range covered by the boot page-table KImage mapping.
#[cfg(target_arch = "loongarch64")]
pub(crate) fn kimage_mapping_range() -> core::ops::Range<usize> {
    kimage_mapping_cover(&kimage_range())
}

#[cfg(any(target_arch = "loongarch64", test))]
fn kimage_mapping_cover(range: &core::ops::Range<usize>) -> core::ops::Range<usize> {
    let size = range
        .len()
        .checked_next_multiple_of(KIMAGE_MAP_ALIGN)
        .expect("kernel image mapping range overflows");
    range.start
        ..range
            .start
            .checked_add(size)
            .expect("kernel image mapping address overflows")
}

fn kimage_reservation_end(end: usize) -> usize {
    end.align_up(crate::consts::PAGE_SIZE)
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
        let desc = MemoryDescriptor::new_with_range(ram_range, MemoryType::BootData);
        add_memory_descriptor(desc).unwrap();
    }
    if let Some(desc) = crate::console::debug_to_memory_desc() {
        #[cfg(target_arch = "loongarch64")]
        unsafe { MEMORY_MAP.update(|map| insert_loongarch_console_memory_descriptor(map, desc)) }
            .unwrap();
        #[cfg(not(target_arch = "loongarch64"))]
        add_memory_descriptor(desc).unwrap();
    }
}

pub fn print_memory_map() {
    println!("Memory Map:");
    // This is also called before heap initialization from early_init().
    unsafe { MEMORY_MAP.update(|m| m.sort_unstable_by_key(|m| m.physical_start)) };

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
    unsafe { MEMORY_MAP.update(|mem| insert_memory_descriptor(mem, desc)) }
}

fn insert_memory_descriptor(
    map: &mut MemoryMap,
    desc: MemoryDescriptor,
) -> Result<(), RangeError<MemoryDescriptor>> {
    map.merge_add(desc)
}

#[cfg(any(target_arch = "loongarch64", test))]
fn insert_loongarch_console_memory_descriptor(
    map: &mut MemoryMap,
    desc: MemoryDescriptor,
) -> Result<(), RangeError<MemoryDescriptor>> {
    let requested = desc.range();
    // The early console reaches MMIO through LoongArch's uncached DMW, so a
    // complete firmware reservation is sufficient and must remain intact.
    if desc.memory_type == MemoryType::Mmio && !requested.is_empty() {
        let mut reserved_cover = false;
        let mut requires_strict_insert = false;
        for existing in map.iter() {
            let existing_range = existing.range();
            if requested.start < existing_range.end && existing_range.start < requested.end {
                if existing.memory_type != MemoryType::Reserved
                    || requested.start < existing_range.start
                    || existing_range.end < requested.end
                    || reserved_cover
                {
                    requires_strict_insert = true;
                    break;
                }
                reserved_cover = true;
            }
        }
        if reserved_cover && !requires_strict_insert {
            return Ok(());
        }
    }

    map.merge_add(desc)
}

pub fn virtual_address_space() -> Result<VirtualAddressSpaceLayout, VirtualAddressSpaceError> {
    Arch::virtual_address_space()
}

/// # Safety
/// Boot CPU only, after installing its own stack, page tables, exception vectors
/// and CPU-local area, before publishing platform RAM lists or starting SMP.
/// No earlier `memory_map()` borrow may remain reachable across this mutation.
#[cfg(all(efi, target_arch = "loongarch64"))]
pub unsafe fn complete_boot_takeover() {
    crate::handoff::received::loongarch64::complete_takeover();
}
#[cfg(not(all(efi, target_arch = "loongarch64")))]
/// # Safety
/// Same early-boot boundary as the inherited EFI implementation.
pub unsafe fn complete_boot_takeover() {}

#[cfg(all(efi, target_arch = "loongarch64"))]
pub(crate) fn replace_boot_map(map: MemoryMap) {
    // SAFETY: the receiver calls once before any platform memory cache is built.
    // Early bootstrap readers no longer retain a borrowed map.
    unsafe { MEMORY_MAP.update(|current| *current = map) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loongarch_console_mmio_inside_reserved_keeps_map_unchanged() {
        let reserved =
            MemoryDescriptor::new_with_range(0x0ee0_0000..0x2000_0000, MemoryType::Reserved);
        let boot_data =
            MemoryDescriptor::new_with_range(0x2000_0000..0x2001_0000, MemoryType::BootData);
        let console_mmio =
            MemoryDescriptor::new_with_range(0x1fe0_0000..0x1fe0_1000, MemoryType::Mmio);

        let mut map = MemoryMap::new();
        for descriptor in [reserved.clone(), boot_data.clone()] {
            insert_memory_descriptor(&mut map, descriptor).unwrap();
        }
        let before = map.clone();

        insert_loongarch_console_memory_descriptor(&mut map, console_mmio.clone()).unwrap();

        assert_eq!(map.as_slice(), before.as_slice());
        assert!(map.iter().any(|descriptor| descriptor == &reserved));
        assert!(map.iter().any(|descriptor| descriptor == &boot_data));

        assert!(matches!(
            insert_memory_descriptor(&mut map, console_mmio),
            Err(RangeError::Conflict { .. })
        ));
        assert_eq!(map.as_slice(), before.as_slice());
    }

    #[test]
    fn loongarch_console_mmio_insertion_only_accepts_reserved_coverage() {
        let reserved =
            MemoryDescriptor::new_with_range(0x1fd0_0000..0x1fe0_0000, MemoryType::Reserved);
        let uncovered_mmio =
            MemoryDescriptor::new_with_range(0x1fe0_0000..0x1fe0_1000, MemoryType::Mmio);
        let mut map = MemoryMap::new();
        insert_memory_descriptor(&mut map, reserved).unwrap();

        insert_loongarch_console_memory_descriptor(&mut map, uncovered_mmio.clone()).unwrap();
        assert!(map.iter().any(|descriptor| descriptor == &uncovered_mmio));

        let overlapping_mmio =
            MemoryDescriptor::new_with_range(0x1fe0_0000..0x1fe0_1000, MemoryType::Mmio);
        for protected in [
            MemoryDescriptor::new_with_range(0x1fe0_0000..0x1fe0_0800, MemoryType::Reserved),
            MemoryDescriptor::new_with_range(0x1fe0_0000..0x1fe0_1000, MemoryType::KImage),
            MemoryDescriptor::new_with_range(0x1fe0_0000..0x1fe0_1000, MemoryType::BootData),
        ] {
            let mut map = MemoryMap::new();
            insert_memory_descriptor(&mut map, protected).unwrap();
            let before = map.clone();

            assert!(matches!(
                insert_loongarch_console_memory_descriptor(&mut map, overlapping_mmio.clone()),
                Err(RangeError::Conflict { .. })
            ));
            assert_eq!(map.as_slice(), before.as_slice());
        }
    }

    #[test]
    fn kimage_reservation_keeps_mapping_padding_owned_by_other_regions() {
        let page = crate::consts::PAGE_SIZE;
        let start = 0xf85d_e000;
        let image_end = start + 0x4b2_345;
        let reserved_end = kimage_reservation_end(image_end);
        let mapping = kimage_mapping_cover(&(start..reserved_end));
        assert!(mapping.end > reserved_end);
        assert_eq!(mapping.len(), 0x60_0000);

        // UEFI ACPI_RECLAIM entries are represented as Reserved by memmap.
        let reserved = MemoryDescriptor::new_with_range(
            reserved_end..reserved_end + page,
            MemoryType::Reserved,
        );
        let archive = MemoryDescriptor::new_with_range(
            reserved_end + page..reserved_end + 3 * page,
            MemoryType::BootData,
        );
        let free = MemoryDescriptor::new_with_range(start..reserved_end, MemoryType::Free);
        let kimage = MemoryDescriptor::new_with_range(start..reserved_end, MemoryType::KImage);

        let mut map = MemoryMap::new();
        for descriptor in [free.clone(), reserved.clone(), archive.clone()] {
            insert_memory_descriptor(&mut map, descriptor).unwrap();
        }
        insert_memory_descriptor(&mut map, kimage.clone()).unwrap();

        assert!(map.iter().any(|entry| entry == &kimage));
        assert!(map.iter().any(|entry| entry == &reserved));
        assert!(map.iter().any(|entry| entry == &archive));

        let old_reservation_end = image_end.align_up(KIMAGE_MAP_ALIGN);
        assert_ne!(old_reservation_end, mapping.end);
        let expanded =
            MemoryDescriptor::new_with_range(start..old_reservation_end, MemoryType::KImage);
        let mut expanded_map = MemoryMap::new();
        for descriptor in [reserved, archive, free] {
            insert_memory_descriptor(&mut expanded_map, descriptor).unwrap();
        }
        assert!(matches!(
            insert_memory_descriptor(&mut expanded_map, expanded),
            Err(RangeError::Conflict { .. })
        ));
    }
}
