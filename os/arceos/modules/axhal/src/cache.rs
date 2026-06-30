//! Cache, TLB, and modified-text synchronization helpers.

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr};

/// Flushes the TLB entries covering a virtual-address range on the current CPU.
pub fn flush_tlb_range(start: VirtAddr, size: usize) {
    for offset in (0..size).step_by(PAGE_SIZE_4K) {
        ax_cpu::asm::flush_tlb(Some(start + offset));
    }
}

/// Flushes the TLB entries covering a virtual-address range on all available CPUs.
pub fn flush_tlb_range_all_cpus(start: VirtAddr, size: usize) {
    #[cfg(feature = "ipi")]
    {
        let _guard = ax_kernel_guard::NoPreempt::new();
        let current_cpu = crate::percpu::this_cpu_id();
        let arg = FlushRangeArg {
            start: start.as_usize(),
            size,
        };
        let arg_ptr = &arg as *const FlushRangeArg as *mut ();

        for cpu_id in 0..crate::cpu_num() {
            if cpu_id == current_cpu {
                continue;
            }
            let _ = unsafe {
                crate::irq::run_on_cpu_sync(
                    crate::irq::CpuId(cpu_id),
                    flush_tlb_range_thunk,
                    arg_ptr,
                )
            };
        }
        flush_tlb_range(start, size);
    }
    #[cfg(not(feature = "ipi"))]
    {
        flush_tlb_range(start, size);
    }
}

#[cfg(feature = "ipi")]
struct FlushRangeArg {
    start: usize,
    size: usize,
}

#[cfg(feature = "ipi")]
unsafe fn flush_tlb_range_thunk(arg: *mut ()) {
    let arg = unsafe { &*(arg as *const FlushRangeArg) };
    flush_tlb_range(VirtAddr::from(arg.start), arg.size);
}

/// Flushes the entire instruction cache on the current CPU.
pub fn flush_icache_all() {
    ax_cpu::asm::flush_icache_all();
}

/// Flushes the entire instruction cache on all available CPUs.
pub fn flush_icache_all_cpus() {
    #[cfg(feature = "ipi")]
    {
        let _guard = ax_kernel_guard::NoPreempt::new();
        let current_cpu = crate::percpu::this_cpu_id();

        for cpu_id in 0..crate::cpu_num() {
            if cpu_id == current_cpu {
                continue;
            }
            let _ = unsafe {
                crate::irq::run_on_cpu_sync(
                    crate::irq::CpuId(cpu_id),
                    flush_icache_all_thunk,
                    core::ptr::null_mut(),
                )
            };
        }
        flush_icache_all();
    }
    #[cfg(not(feature = "ipi"))]
    {
        flush_icache_all();
    }
}

#[cfg(feature = "ipi")]
unsafe fn flush_icache_all_thunk(_arg: *mut ()) {
    flush_icache_all();
}

/// Cleans a data-cache range to the point of unification when needed.
pub fn clean_dcache_to_pou(vaddr: VirtAddr, size: usize) {
    #[cfg(target_arch = "aarch64")]
    {
        ax_cpu::asm::clean_dcache_range_to_pou(vaddr, size);
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = (vaddr, size);
    }
}

/// Synchronizes modified kernel text with the local execution pipeline.
pub fn sync_kernel_text(start: VirtAddr, size: usize) {
    flush_tlb_range(start, size);
    flush_icache_all();
}
