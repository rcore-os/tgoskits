use core::ops::Range;

use byte_unit::{Byte, UnitType};
use kernutil::StaticCell;
pub use kernutil::memory::{MemoryDescriptor, MemoryType, PageTableInfo};
use num_align::NumAlign;
use ranges_ext::*;

pub mod mmu;
pub(crate) mod no_map;
pub(crate) mod ram;
pub(crate) mod region;

pub use mmu::{MemAttributes, PteConfig};
pub use no_map::{NoMapError, NoMapRegions, reserved_ram_regions};
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

const BOOT_INPUT_MAP_CAPACITY: usize = 512;
#[cfg(any(all(efi, target_arch = "riscv64"), test))]
// The FDT copy, per-CPU areas, and boot page tables consume three
// successive prefixes of a free bank. Early-console MMIO can split
// the remaining free range into two, adding up to two more entries.
const EFI_LATE_BOOT_SLOTS: usize = 5;
#[cfg(any(all(efi, target_arch = "riscv64"), test))]
pub(crate) const MEMORY_MAP_CAPACITY: usize = BOOT_INPUT_MAP_CAPACITY + EFI_LATE_BOOT_SLOTS;
#[cfg(not(any(all(efi, target_arch = "riscv64"), test)))]
pub(crate) const MEMORY_MAP_CAPACITY: usize = BOOT_INPUT_MAP_CAPACITY;

/// Bound for the normalized mappings without architecture-specific additions.
/// Each disjoint no-map range splits at most one descriptor, and reclaimable
/// initramfs backing can also appear in the RAM view.
pub const MAX_PHYS_MEMORY_REGIONS: usize = MEMORY_MAP_CAPACITY + no_map::MAX_REGIONS + 1;

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
    let excluded = NoMapRegions::current().expect("invalid FDT no-map exclusions");
    required_boot_mappings(&excluded, kimage_range(), 0..0)
        .expect("kernel mapping overlaps FDT no-map pages");
    #[cfg(efi)]
    crate::efi_stub::init_boot_memory_map();
    #[cfg(not(efi))]
    crate::fdt::init_memory_map();

    let kernel_range = kimage_range();
    let result = add_memory_descriptor(kimage_descriptor(kernel_range.clone()));
    if let Err(error) = result {
        #[cfg(target_arch = "loongarch64")]
        println!(
            "KImage reservation failed: range={kernel_range:#x?}, entries={}, error={error:?}",
            memory_map().len(),
        );
        panic!("failed to add KImage memory descriptor {kernel_range:#x?}: {error:?}");
    }
    reserve_arch_early_ranges();

    check_required_memory(memory_map(), &excluded).expect("invalid required boot mapping");

    let free_range = ram::select_range(memory_map(), |_| true).expect("No free memory");
    begin_early_allocation(free_range, &excluded);
}

fn check_required_memory(
    map: &[MemoryDescriptor],
    excluded: &NoMapRegions,
) -> Result<(), NoMapError> {
    for descriptor in map {
        if matches!(
            descriptor.memory_type,
            MemoryType::KImage
                | MemoryType::PerCpuData
                | MemoryType::BootData
                | MemoryType::BootReclaimable
        ) {
            let end = descriptor
                .physical_start
                .checked_add(descriptor.size_in_bytes)
                .ok_or(NoMapError::InvalidRange)?;
            excluded.ensure_mappable(descriptor.physical_start..end)?;
        }
    }
    Ok(())
}

pub(crate) fn begin_early_allocation(free_range: Range<usize>, excluded: &NoMapRegions) {
    print_memory_map();

    ram::init(free_range);
    crate::fdt::save_fdt(excluded);
    crate::smp::alloc_percpu();

    // Allocation reservations can split RAM descriptors and move the new
    // pieces to the end. Keep the mapping walk in the same order as preflight.
    #[cfg(all(efi, target_arch = "riscv64"))]
    unsafe {
        MEMORY_MAP.update(|map| map.sort_unstable_by_key(|entry| entry.physical_start));
    }
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

fn kimage_descriptor(range: Range<usize>) -> MemoryDescriptor {
    MemoryDescriptor::new_with_range(
        range.start..kimage_reservation_end(range.end),
        MemoryType::KImage,
    )
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn kimage_is_initialized() -> bool {
    // SAFETY: primary setup publishes these immutable image bounds before
    // starting other CPUs. The EFI entry can query them before publication.
    let image_start = unsafe { KIMAGE_START };
    image_start.is_some()
}

/// Includes the entire large-page tail mapped by the RISC-V bootstrap.
pub(crate) fn required_boot_mappings(
    excluded: &NoMapRegions,
    range: Range<usize>,
    percpu: Range<usize>,
) -> Result<Range<usize>, NoMapError> {
    let size = range
        .end
        .checked_sub(range.start)
        .ok_or(NoMapError::InvalidRange)?;
    let size = size
        .checked_next_multiple_of(KIMAGE_MAP_ALIGN)
        .ok_or(NoMapError::InvalidRange)?;
    let kernel = range.start
        ..range
            .start
            .checked_add(size)
            .ok_or(NoMapError::InvalidRange)?;
    excluded.ensure_mappable(kernel.clone())?;
    if percpu.start > percpu.end {
        return Err(NoMapError::InvalidRange);
    }
    if !percpu.is_empty() {
        excluded.ensure_mappable(percpu)?;
    }
    Ok(kernel)
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

pub(crate) fn insert_memory_descriptor(
    map: &mut MemoryMap,
    mut desc: MemoryDescriptor,
) -> Result<(), RangeError<MemoryDescriptor>> {
    let range = desc.range();
    if let Some(neighbor) = map.iter().find(|entry| {
        let existing = entry.range();
        entry.mergeable(&desc) && existing.end >= range.start && range.end >= existing.start
    }) {
        // Cover an existing slot so merge_add can reuse it on a full map.
        let existing = neighbor.range();
        desc = desc.clone_with_range(range.start.min(existing.start)..range.end.max(existing.end));
    }
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

pub(crate) fn reserve_fdt_memory(fdt: &fdt_raw::Fdt<'_>) -> Result<(), NoMapError> {
    let archive = if crate::boot_payload::initramfs_range().is_none() {
        crate::fdt::direct_boot_initramfs(fdt)?
    } else {
        None
    };
    // SAFETY: FDT reservation runs once on the boot CPU before RAM allocation.
    let (_, archive) = unsafe { MEMORY_MAP.update(|map| reserve_fdt_layout(map, fdt, archive)) }?;
    if let Some(archive) = archive {
        crate::boot_payload::publish(archive.start, archive.end, archive.reclaimable);
    }
    Ok(())
}

fn reserve_fdt_layout(
    map: &mut MemoryMap,
    fdt: &fdt_raw::Fdt<'_>,
    archive: Option<crate::boot_payload::InitramfsRange>,
) -> Result<(NoMapRegions, Option<crate::boot_payload::InitramfsRange>), NoMapError> {
    let excluded = crate::fdt::reserve_static_memory(map, fdt)?;
    let archive = archive
        .map(|archive| crate::fdt::reserve_fdt_initramfs(map, archive))
        .transpose()?;
    Ok((excluded, archive))
}

#[cfg(any(all(efi, target_arch = "riscv64"), test))]
#[derive(Debug)]
pub(crate) struct PreparedBootMemory {
    pub(crate) excluded: NoMapRegions,
    pub(crate) free_range: Range<usize>,
    pub(crate) archive: Option<crate::boot_payload::InitramfsRange>,
}

#[cfg(any(all(efi, target_arch = "riscv64"), test))]
pub(crate) fn check_fdt_boot_map(
    map: &mut MemoryMap,
    fdt: &fdt_raw::Fdt<'_>,
    kernel: Range<usize>,
    boot_reservations: impl IntoIterator<Item = MemoryDescriptor>,
    archive: Option<crate::boot_payload::InitramfsRange>,
    select_free_range: impl FnOnce(
        &[MemoryDescriptor],
        &NoMapRegions,
        Range<usize>,
    ) -> Result<Range<usize>, NoMapError>,
) -> Result<PreparedBootMemory, NoMapError> {
    let reservation_error = |error| match error {
        RangeError::Capacity => NoMapError::Capacity,
        RangeError::Conflict { .. } => NoMapError::ReservationConflict,
    };
    for reservation in boot_reservations {
        insert_memory_descriptor(map, reservation).map_err(reservation_error)?;
    }
    let (excluded, archive) = reserve_fdt_layout(map, fdt, archive)?;
    insert_memory_descriptor(map, kimage_descriptor(kernel.clone())).map_err(reservation_error)?;
    if map.len() > BOOT_INPUT_MAP_CAPACITY {
        return Err(NoMapError::Capacity);
    }
    let kernel = required_boot_mappings(&excluded, kernel, 0..0)?;
    check_required_memory(map, &excluded)?;
    map.sort_unstable_by_key(|entry| entry.physical_start);
    let free_range = select_free_range(map, &excluded, kernel)?;
    Ok(PreparedBootMemory {
        excluded,
        free_range,
        archive,
    })
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

#[cfg(all(efi, any(target_arch = "riscv64", target_arch = "loongarch64")))]
pub(crate) fn replace_boot_map(map: MemoryMap) {
    // SAFETY: the receiver calls once before any platform memory cache is built.
    // Early bootstrap readers no longer retain a borrowed map.
    unsafe { MEMORY_MAP.update(|current| *current = map) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fdt_boot_map_accounts_for_kernel_and_initramfs_splits() {
        use fdt_edit::{Fdt, Node, Property};

        use crate::consts::PAGE_SIZE;

        let bytes = Fdt::new().encode();
        let fdt = fdt_raw::Fdt::from_bytes(bytes.as_ref()).unwrap();
        let kernel_start = BOOT_INPUT_MAP_CAPACITY * 2 * PAGE_SIZE;
        let kernel = kernel_start..kernel_start + PAGE_SIZE;
        let mut map = MemoryMap::new();
        for index in 0..BOOT_INPUT_MAP_CAPACITY - 2 {
            let start = index * 2 * PAGE_SIZE;
            map.push(MemoryDescriptor::new_with_range(
                start..start + PAGE_SIZE,
                MemoryType::Reserved,
            ))
            .unwrap();
        }
        map.push(MemoryDescriptor::new_with_range(
            kernel.start - 4 * PAGE_SIZE..kernel.end + 16 * MB,
            MemoryType::Free,
        ))
        .unwrap();
        let select_ram = |map: &[MemoryDescriptor], _: &NoMapRegions, _: Range<usize>| {
            ram::select_range(map, |_| true).ok_or(NoMapError::Capacity)
        };
        let mut preview = map.clone();
        assert_eq!(
            check_fdt_boot_map(&mut preview, &fdt, kernel.clone(), [], None, select_ram)
                .unwrap_err(),
            NoMapError::Capacity
        );
        map.remove(0);
        let mut without_initramfs = map.clone();
        let prepared = check_fdt_boot_map(
            &mut without_initramfs,
            &fdt,
            kernel.clone(),
            [],
            None,
            select_ram,
        )
        .unwrap();
        assert_eq!(without_initramfs.len(), BOOT_INPUT_MAP_CAPACITY);
        assert!(prepared.archive.is_none());
        assert!(prepared.excluded.ranges().is_empty());
        assert_eq!(prepared.free_range, kernel.end..kernel.end + 16 * MB);

        let initramfs_start = kernel.start - 3 * PAGE_SIZE;
        let initramfs = MemoryDescriptor::new_aligned(
            initramfs_start,
            PAGE_SIZE - 1,
            MemoryType::BootData,
            PAGE_SIZE,
        );
        let mut preview = map.clone();
        assert_eq!(
            check_fdt_boot_map(
                &mut preview,
                &fdt,
                kernel.clone(),
                [initramfs.clone()],
                None,
                select_ram
            )
            .unwrap_err(),
            NoMapError::Capacity
        );

        let mut tree = Fdt::new();
        let root = tree.root_id();
        let chosen = tree.add_node(root, Node::new("chosen"));
        let chosen = tree.node_mut(chosen).unwrap();
        for (name, value) in [
            ("linux,initrd-start", initramfs_start),
            ("linux,initrd-end", initramfs_start + PAGE_SIZE - 1),
        ] {
            chosen.set_property(Property::new(name, (value as u64).to_be_bytes().to_vec()));
        }
        let bytes = tree.encode();
        let fallback = fdt_raw::Fdt::from_bytes(bytes.as_ref()).unwrap();
        assert!(crate::fdt::direct_boot_initramfs(&fallback).is_err());
        #[cfg(any(efi, not(target_arch = "riscv64")))]
        let archive = {
            use uefi::boot::{MemoryDescriptor as EfiDescriptor, MemoryType as EfiType};

            let loader_ram = EfiDescriptor {
                phys_start: initramfs_start as u64,
                page_count: 1,
                ty: EfiType::LOADER_DATA,
                ..Default::default()
            };
            let from_map = |entry| {
                crate::fdt::initramfs_from_fdt(fallback.clone(), |range| {
                    crate::efi_memory_map::covers_initramfs_ram(core::iter::once(entry), range)
                })
            };
            assert!(
                from_map(EfiDescriptor {
                    ty: EfiType::UNUSABLE,
                    ..loader_ram
                })
                .is_err()
            );
            assert!(
                from_map(EfiDescriptor {
                    phys_start: loader_ram.phys_start + PAGE_SIZE as u64,
                    ..loader_ram
                })
                .is_err()
            );
            from_map(loader_ram).unwrap()
        };
        #[cfg(all(not(efi), target_arch = "riscv64"))]
        let archive = crate::fdt::initramfs_from_fdt(fallback.clone(), |_| true).unwrap();
        assert_eq!(
            check_fdt_boot_map(
                &mut map.clone(),
                &fallback,
                kernel.clone(),
                [],
                archive,
                select_ram
            )
            .unwrap_err(),
            NoMapError::Capacity
        );

        map.remove(0);
        map.remove(0);
        check_fdt_boot_map(
            &mut map,
            &fdt,
            kernel.clone(),
            [initramfs.clone()],
            None,
            select_ram,
        )
        .unwrap();
        assert_eq!(map.len(), BOOT_INPUT_MAP_CAPACITY);
        assert!(map.iter().any(|entry| *entry == initramfs));
        assert!(
            map.iter().any(|entry| {
                entry.memory_type == MemoryType::KImage && entry.range() == kernel
            })
        );

        // The early allocator consumes prefixes from the remaining free bank.
        // A console window in its interior can split the tail into two.
        for (offset, kind) in [
            (0, MemoryType::Reserved),
            (1, MemoryType::PerCpuData),
            (2, MemoryType::BootData),
            (4, MemoryType::Mmio),
        ] {
            let start = kernel.end + offset * PAGE_SIZE;
            insert_memory_descriptor(
                &mut map,
                MemoryDescriptor::new_with_range(start..start + PAGE_SIZE, kind),
            )
            .unwrap();
        }
        assert_eq!(map.len(), MEMORY_MAP_CAPACITY);
    }

    #[test]
    fn early_ram_selection_requires_room_for_aligned_boot_allocations() {
        use core::alloc::Layout;

        let page_bytes = crate::consts::PAGE_SIZE;
        let first = 32 * MB..41 * MB;
        let second = 64 * MB..96 * MB;
        let map = [first.clone(), second.clone()]
            .map(|range| MemoryDescriptor::new_with_range(range, MemoryType::Free));
        let allocations = [
            Layout::from_size_align(10 * MB, 8).unwrap(),
            Layout::from_size_align(MB, MB).unwrap(),
            Layout::from_size_align(page_bytes, page_bytes).unwrap(),
        ];

        let fits = |range: &Range<usize>| {
            allocations
                .iter()
                .try_fold(ram::initial_cursor(range.start), |cursor, layout| {
                    ram::planned_allocation(cursor, *layout, page_bytes).map(|range| range.end)
                })
                .is_some_and(|end| end <= range.end)
        };
        assert_eq!(ram::select_range(&map, fits), Some(second));
        assert_eq!(ram::select_range(&map[..1], fits), None);
        assert_eq!(ram::select_range(&map, |_| true), Some(first));
    }

    #[test]
    fn memory_descriptor_can_extend_a_full_map() {
        let mut map = MemoryMap::new();
        for index in 0..MEMORY_MAP_CAPACITY {
            let start = index * 3 * crate::consts::PAGE_SIZE;
            map.push(MemoryDescriptor::new_with_range(
                start..start + crate::consts::PAGE_SIZE,
                MemoryType::Reserved,
            ))
            .unwrap();
        }
        let end = map.last().unwrap().range().end;
        let new_end = end + crate::consts::PAGE_SIZE;
        let continuation = MemoryDescriptor::new_with_range(end..new_end, MemoryType::Reserved);
        insert_memory_descriptor(&mut map, continuation).unwrap();
        assert_eq!(map.len(), MEMORY_MAP_CAPACITY);
        assert_eq!(
            map.iter().map(|entry| entry.range().end).max(),
            Some(new_end)
        );
    }

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
