use ax_plat::mem::{IomapAttrs, IomapDecision, IomapError, MemIf, PhysAddr, RawRange, VirtAddr};
use heapless::Vec;
use someboot::ArchTrait;
use somehal::mem::MemoryType;
use spin::Once;

static FREE_LIST: Once<Vec<RawRange, 32>> = Once::new();
static RESERVED_LIST: Once<Vec<RawRange, 32>> = Once::new();
static MMIO_LIST: Once<Vec<RawRange, 16>> = Once::new();

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
}

#[unsafe(no_mangle)]
fn _percpu_base_ptr(idx: usize) -> *mut u8 {
    somehal::smp::percpu_data_ptr(idx).unwrap_or_default()
}
