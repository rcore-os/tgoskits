use core::sync::atomic::{AtomicUsize, Ordering};

use ax_lazyinit::LazyInit;
use ax_plat::mem::{MemIf, PhysAddr, RawRange, VirtAddr, pa, va};
use fdt_raw::Fdt;

use crate::config::{
    devices::MMIO_RANGES,
    plat::{
        HIGH_MEMORY_BASE, LOW_MEMORY_BASE, LOW_MEMORY_SIZE, PHYS_BOOT_OFFSET, PHYS_MEMORY_SIZE,
        PHYS_VIRT_OFFSET,
    },
};

/// QEMU's boot argument registers (`$a0..$a3` at kernel entry), captured by
/// `boot.rs` before paging. QEMU passes the FDT physical pointer in one of them
/// (which one varies by qemu version/boot path), so [`detect_ram`] probes all
/// four plus a scan of the low reserved region. All `0` = none captured.
static BOOT_REGS: [AtomicUsize; 4] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

/// Called once from `_start` (boot.rs) with the raw boot argument registers.
/// `extern "C"` so the boot naked-asm can `jirl` here directly. Only stores the
/// integers — never dereferences (the FDT is read later, post-MMU, in detect_ram).
#[unsafe(no_mangle)]
pub(crate) extern "C" fn set_fdt_ptr(a0: usize, a1: usize, a2: usize, a3: usize) {
    BOOT_REGS[0].store(a0, Ordering::Relaxed);
    BOOT_REGS[1].store(a1, Ordering::Relaxed);
    BOOT_REGS[2].store(a2, Ordering::Relaxed);
    BOOT_REGS[3].store(a3, Ordering::Relaxed);
}

/// Detected RAM banks. Fixed-capacity (qemu-virt loong has 1-2 banks); avoids
/// pulling in an alloc/heapless dependency.
const MAX_RAM_RANGES: usize = 8;
struct RamRanges {
    arr: [RawRange; MAX_RAM_RANGES],
    len: usize,
}
static RAM_RANGES: LazyInit<RamRanges> = LazyInit::new();

/// Device-tree blob magic (`0xd00dfeed`, stored big-endian on disk).
const FDT_MAGIC: u32 = 0xd00d_feed;

/// Const fallback mirroring the original static layout, used when the FDT is
/// absent/unparseable. `PHYS_MEMORY_SIZE` is the safe 512 MiB default.
fn fallback_ranges(arr: &mut [RawRange; MAX_RAM_RANGES]) -> usize {
    const HIGH_MEMORY_SIZE: usize = PHYS_MEMORY_SIZE.saturating_sub(LOW_MEMORY_SIZE);
    if HIGH_MEMORY_SIZE == 0 {
        arr[0] = (LOW_MEMORY_BASE, PHYS_MEMORY_SIZE);
        1
    } else {
        arr[0] = (LOW_MEMORY_BASE, LOW_MEMORY_SIZE);
        arr[1] = (HIGH_MEMORY_BASE, HIGH_MEMORY_SIZE);
        2
    }
}

/// Try to parse an FDT at physical address `phys` and fill `arr` with its
/// `/memory` regions. Returns the count (0 = not a valid FDT / no memory node).
///
/// Reads go through the boot direct-map window (`PHYS_BOOT_OFFSET`, set at the very
/// start of `_start` via DMWIN0) which is always valid regardless of page-table
/// setup. Only low-RAM, aligned addresses are probed so a bogus register value
/// (e.g. a cpuid) can never dereference an unmapped address.
fn try_fdt_at(phys: usize, arr: &mut [RawRange; MAX_RAM_RANGES]) -> usize {
    if !(0x1000..LOW_MEMORY_SIZE).contains(&phys) || !phys.is_multiple_of(4) {
        return 0;
    }
    let vaddr = phys + PHYS_BOOT_OFFSET;
    // cheap magic gate before invoking the full parser
    let magic = unsafe { core::ptr::read_volatile(vaddr as *const u32) };
    if u32::from_be(magic) != FDT_MAGIC {
        return 0;
    }
    // SAFETY: `vaddr` is in the boot direct-map window and has passed the FDT
    // magic gate above; `fdt-raw` (the workspace-standard reader, also used by
    // someboot::fdt) re-validates the header/structure and errors out otherwise.
    let Ok(fdt) = (unsafe { Fdt::from_ptr(vaddr as *mut u8) }) else {
        return 0;
    };
    let mut n = 0;
    for mem in fdt.memory() {
        for region in mem.regions() {
            if region.size == 0 || n >= MAX_RAM_RANGES {
                continue;
            }
            arr[n] = (region.address as usize, region.size as usize);
            n += 1;
        }
    }
    n
}

/// Parse the FDT QEMU handed us so the kernel honors the real RAM size (qemu
/// `-m`) instead of a hardcoded constant. Probes the captured boot registers,
/// then scans the reserved low region for the FDT magic, then falls back to the
/// const layout — so boot can never regress.
fn detect_ram() -> RamRanges {
    let mut arr = [(0usize, 0usize); MAX_RAM_RANGES];
    let mut len = 0usize;

    // 1) FDT pointer is in one of a0..a3.
    for reg in &BOOT_REGS {
        len = try_fdt_at(reg.load(Ordering::Relaxed), &mut arr);
        if len > 0 {
            break;
        }
    }

    // 2) Otherwise scan the reserved low region ("boot_info + fdt", [0, 0x200000])
    //    for the FDT magic, in case it is placed at a fixed location not a register.
    if len == 0 {
        let mut p = 0usize;
        while p < 0x20_0000 {
            len = try_fdt_at(p, &mut arr);
            if len > 0 {
                break;
            }
            p += 8;
        }
    }

    // 3) Const fallback (512 MiB) when no FDT is found.
    if len == 0 {
        len = fallback_ranges(&mut arr);
    }
    RamRanges { arr, len }
}

struct MemIfImpl;

#[impl_plat_interface]
impl MemIf for MemIfImpl {
    /// Returns all physical memory (RAM) ranges on the platform.
    ///
    /// All memory ranges except reserved ranges (including the kernel loaded
    /// range) are free for allocation.
    fn phys_ram_ranges() -> &'static [RawRange] {
        // Honor the real RAM size from the DTB (qemu `-m`); fall back to the
        // const `axconfig.toml` layout if the FDT is absent/unparseable.
        let r = RAM_RANGES.get_or_init(detect_ram);
        &r.arr[..r.len]
    }

    /// Returns all reserved physical memory ranges on the platform.
    ///
    /// Reserved memory can be contained in [`phys_ram_ranges`], they are not
    /// allocatable but should be mapped to kernel's address space.
    ///
    /// Note that the ranges returned should not include the range where the
    /// kernel is loaded.
    fn reserved_phys_ram_ranges() -> &'static [RawRange] {
        // AxVisor is loaded from high memory on LoongArch. Keep the low RAM
        // bank out of the host allocator so passthrough devices can DMA to
        // guest identity-mapped low memory.
        &[(LOW_MEMORY_BASE, LOW_MEMORY_SIZE)]
    }

    /// Returns all device memory (MMIO) ranges on the platform.
    fn mmio_ranges() -> &'static [RawRange] {
        &MMIO_RANGES
    }

    /// Translates a physical address to a virtual address.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        va!(paddr.as_usize() + PHYS_VIRT_OFFSET)
    }

    /// Translates a virtual address to a physical address.
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        let vaddr = vaddr.as_usize();
        if vaddr & 0xffff_0000_0000_0000 == PHYS_BOOT_OFFSET {
            pa!(vaddr - PHYS_BOOT_OFFSET)
        } else {
            pa!(vaddr - PHYS_VIRT_OFFSET)
        }
    }

    /// Returns the kernel address space base virtual address and size.
    fn kernel_aspace() -> (VirtAddr, usize) {
        (
            va!(crate::config::plat::KERNEL_ASPACE_BASE),
            crate::config::plat::KERNEL_ASPACE_SIZE,
        )
    }
}
