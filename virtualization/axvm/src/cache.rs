//! Host cache maintenance helpers used by VM image loading.

use ax_memory_addr::VirtAddr;

/// Clean data cache lines covering a host virtual address range.
pub fn clean_dcache_range(addr: VirtAddr, size: usize) {
    #[cfg(target_arch = "aarch64")]
    {
        aarch64_cpu_ext::cache::dcache_range(
            aarch64_cpu_ext::cache::CacheOp::Clean,
            addr.as_usize(),
            size,
        );
    }

    #[cfg(target_arch = "loongarch64")]
    unsafe {
        cache_range::<DCACHE_WB>(addr, size);
        core::arch::asm!("dbar 0");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "riscv64"))]
    {
        let _ = (addr, size);
    }
}

#[cfg(target_arch = "loongarch64")]
const CACHE_LINE_SIZE: usize = 64;
#[cfg(target_arch = "loongarch64")]
const DCACHE_WB: u8 = 0x19;

#[cfg(target_arch = "loongarch64")]
unsafe fn cache_range<const OP: u8>(addr: VirtAddr, size: usize) {
    if size == 0 {
        return;
    }

    let start = addr.as_usize() & !(CACHE_LINE_SIZE - 1);
    let end = addr.as_usize() + size;
    let mut current = start;

    while current < end {
        unsafe {
            core::arch::asm!("cacop {0}, {1}, 0", const OP, in(reg) current);
        }
        current += CACHE_LINE_SIZE;
    }
}
