use ax_plat::mem::{
    DCacheOp, IomapAttrs, IomapDecision, IomapError, MemIf, PhysAddr, RawRange, VirtAddr,
};
use heapless::Vec;
use someboot::ArchTrait;
use somehal::mem::MemoryType;
use spin::Once;

static FREE_LIST: Once<Vec<RawRange, 32>> = Once::new();
static RESERVED_LIST: Once<Vec<RawRange, 32>> = Once::new();
static MMIO_LIST: Once<Vec<RawRange, 16>> = Once::new();

/// One immutable physical RAM range retained for early host-allocator exclusion.
///
/// Axvisor's build output places values of this type in `ax_reserved_phys_ram`. The dynamic
/// platform consumes that section before the global allocator is initialized, so fixed guest RAM
/// cannot be reused for host objects.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EarlyReservedPhysRamRange {
    start: usize,
    size: usize,
}

impl EarlyReservedPhysRamRange {
    /// Creates one immutable early-boot host RAM reservation.
    pub const fn new(start: usize, size: usize) -> Self {
        Self { start, size }
    }
}

/// Returns physical RAM ranges reserved by the image before host allocation begins.
///
/// These ranges are suitable for validating embedded startup VM memory during construction. The
/// iterator does not grant ownership; the VM lifecycle must still prevent two live guests from
/// claiming the same backing.
pub fn early_reserved_phys_ram_ranges() -> impl Iterator<Item = RawRange> {
    linked_reserved_phys_ram_ranges()
        .iter()
        .filter_map(|range| (range.size != 0).then_some((range.start, range.size)))
}

#[used]
#[unsafe(link_section = "ax_reserved_phys_ram")]
static LINKED_RESERVED_PHYS_RAM_SENTINEL: EarlyReservedPhysRamRange =
    EarlyReservedPhysRamRange::new(0, 0);

fn linked_reserved_phys_ram_ranges() -> &'static [EarlyReservedPhysRamRange] {
    unsafe extern "C" {
        static __start_ax_reserved_phys_ram: EarlyReservedPhysRamRange;
        static __stop_ax_reserved_phys_ram: EarlyReservedPhysRamRange;
    }

    let start = core::ptr::addr_of!(__start_ax_reserved_phys_ram) as usize;
    let end = core::ptr::addr_of!(__stop_ax_reserved_phys_ram) as usize;
    let entry_size = core::mem::size_of::<EarlyReservedPhysRamRange>();
    let Some(byte_len) = end.checked_sub(start) else {
        return &[];
    };
    if !byte_len.is_multiple_of(entry_size) {
        return &[];
    }

    // SAFETY: every input item in `ax_reserved_phys_ram` has this `repr(C)` two-usize ABI. The
    // linker-provided bounds delimit the retained section for the lifetime of the image, and the
    // size check above rejects trailing bytes rather than constructing a partial entry.
    unsafe {
        core::slice::from_raw_parts(
            start as *const EarlyReservedPhysRamRange,
            byte_len / entry_size,
        )
    }
}

#[cfg(target_arch = "x86_64")]
const X86_FIXED_MMIO_RANGES: &[RawRange] = &[
    (0xfec0_0000, 0x1000), // IOAPIC
    (0xfed0_0000, 0x1000), // HPET
    (0xfee0_0000, 0x1000), // LAPIC
];

#[cfg(target_arch = "x86_64")]
const X86_RESERVED_RAM_RANGES: &[RawRange] = &[
    // Match the static q35 platform: the low 2 MiB contains legacy holes and
    // boot-time data such as the AP trampoline, and must not enter the heap.
    (0, 0x20_0000),
];

#[cfg(target_arch = "loongarch64")]
const LOONGARCH_RESERVED_RAM_RANGES: &[RawRange] = &[
    // Keep the low RAM identity-mappable for passthrough DMA used by the
    // LoongArch QEMU virt machine.
    (0, 0x1000_0000),
];

struct MemIfImpl;

fn push_non_overlapping<const N: usize>(list: &mut Vec<RawRange, N>, range: RawRange) {
    let (start, size) = range;
    if size == 0 {
        return;
    }

    list.sort_unstable_by_key(|&(start, _)| start);
    let original_len = list.len();
    let mut cursor = start;
    let end = start.saturating_add(size);
    for index in 0..original_len {
        let (existing_start, existing_size) = list[index];
        let existing_end = existing_start.saturating_add(existing_size);
        if existing_end <= cursor {
            continue;
        }
        if existing_start >= end {
            break;
        }
        if existing_start > cursor {
            list.push((cursor, existing_start - cursor)).unwrap();
        }
        cursor = cursor.max(existing_end);
        if cursor >= end {
            break;
        }
    }

    if cursor < end {
        list.push((cursor, end - cursor)).unwrap();
    }
    list.sort_unstable_by_key(|&(start, _)| start);
}

#[impl_plat_interface]
impl MemIf for MemIfImpl {
    fn phys_ram_ranges() -> &'static [RawRange] {
        FREE_LIST.call_once(|| {
            let mut list = Vec::new();
            for r in somehal::mem::memory_map() {
                if matches!(r.memory_type, MemoryType::Free) {
                    list.push((r.physical_start, r.size_in_bytes)).unwrap();
                }
            }
            list
        })
    }

    fn reserved_phys_ram_ranges() -> &'static [RawRange] {
        RESERVED_LIST.call_once(|| {
            let mut list = Vec::new();
            for r in somehal::mem::memory_map() {
                if matches!(
                    r.memory_type,
                    MemoryType::Reserved | MemoryType::KImage | MemoryType::PerCpuData
                ) {
                    push_non_overlapping(&mut list, (r.physical_start, r.size_in_bytes));
                }
            }
            for reservation in linked_reserved_phys_ram_ranges() {
                push_non_overlapping(&mut list, (reservation.start, reservation.size));
            }
            #[cfg(target_arch = "x86_64")]
            for &range in X86_RESERVED_RAM_RANGES {
                push_non_overlapping(&mut list, range);
            }
            #[cfg(target_arch = "loongarch64")]
            for &range in LOONGARCH_RESERVED_RAM_RANGES {
                push_non_overlapping(&mut list, range);
            }
            list
        })
    }

    fn mmio_ranges() -> &'static [RawRange] {
        MMIO_LIST.call_once(|| {
            let mut list = Vec::new();
            for r in somehal::mem::memory_map() {
                if matches!(r.memory_type, MemoryType::Mmio) {
                    push_non_overlapping(&mut list, (r.physical_start, r.size_in_bytes));
                }
            }
            #[cfg(target_arch = "x86_64")]
            for &range in X86_FIXED_MMIO_RANGES {
                // QEMU/OVMF does not always report fixed PC chipset MMIO holes.
                push_non_overlapping(&mut list, range);
            }
            list
        })
    }

    fn prepare_iomap(
        addr: PhysAddr,
        size: usize,
        attrs: IomapAttrs,
    ) -> Result<IomapDecision, IomapError> {
        if size == 0 {
            return Err(IomapError::InvalidInput);
        }
        let paddr: PhysAddr =
            <someboot::arch::Arch as ArchTrait>::canonicalize_paddr(addr.as_usize()).into();
        if attrs == IomapAttrs::DEVICE
            && let Some(vaddr) =
                <someboot::arch::Arch as ArchTrait>::ioremap_device(paddr.as_usize(), size)
        {
            return Ok(IomapDecision::Mapped((vaddr as usize).into()));
        }
        Ok(IomapDecision::UseGeneric(paddr))
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        (somehal::mem::phys_to_virt(paddr.as_usize()) as usize).into()
    }

    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        somehal::mem::virt_to_phys(vaddr.as_ptr()).into()
    }

    fn kernel_aspace() -> (VirtAddr, usize) {
        let range = somehal::mem::kernel_space();
        (range.start.into(), range.len())
    }

    fn user_aspace_needs_kernel_mappings() -> bool {
        <someboot::arch::Arch as ArchTrait>::user_aspace_needs_kernel_mappings()
    }

    fn dcache_range(op: DCacheOp, addr: VirtAddr, size: usize) {
        somehal::cache::dcache_range(to_somehal_dcache_op(op), addr.as_usize() as *const u8, size);
    }

    fn dma_coherent_before_make_uncached(addr: VirtAddr, size: usize) {
        somehal::cache::dma_coherent_before_make_uncached(addr.as_usize() as *const u8, size);
    }

    fn dma_coherent_before_restore_cached(addr: VirtAddr, size: usize) {
        somehal::cache::dma_coherent_before_restore_cached(addr.as_usize() as *const u8, size);
    }

    fn dma_coherent_after_mapping_update() {
        somehal::cache::dma_coherent_after_mapping_update();
    }
}

fn to_somehal_dcache_op(op: DCacheOp) -> somehal::cache::DCacheOp {
    match op {
        DCacheOp::Clean => somehal::cache::DCacheOp::Clean,
        DCacheOp::Invalidate => somehal::cache::DCacheOp::Invalidate,
        DCacheOp::CleanInvalidate => somehal::cache::DCacheOp::CleanInvalidate,
    }
}

#[unsafe(no_mangle)]
fn _percpu_base_ptr(idx: usize) -> *mut u8 {
    somehal::smp::percpu_data_ptr(idx).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_LINKED_RESERVATION: EarlyReservedPhysRamRange =
        EarlyReservedPhysRamRange::new(0x1_8000_0000, 0x2000_0000);

    #[used]
    #[unsafe(link_section = "ax_reserved_phys_ram")]
    static LINKED_RESERVED_PHYS_RAM_TEST_ENTRY: EarlyReservedPhysRamRange = TEST_LINKED_RESERVATION;

    #[test]
    fn discovers_linked_early_physical_reservations() {
        assert!(early_reserved_phys_ram_ranges().any(|range| {
            range == (TEST_LINKED_RESERVATION.start, TEST_LINKED_RESERVATION.size)
        }));
    }
}
