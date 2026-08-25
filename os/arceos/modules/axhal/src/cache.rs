//! Cache, TLB, and modified-text synchronization helpers.

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};

// The range API is normalized to 4 KiB pages. x86_64 and RISC-V use the
// current Linux defaults; the other backends keep the page-table engine's
// existing 32-entry bound until an architecture-specific cost model exists.
#[cfg(target_arch = "x86_64")]
const TLB_SINGLE_PAGE_FLUSH_CEILING: usize = 33;
#[cfg(target_arch = "riscv64")]
const TLB_SINGLE_PAGE_FLUSH_CEILING: usize = 64;
#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
const TLB_SINGLE_PAGE_FLUSH_CEILING: usize = 32;
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64"
)))]
const TLB_SINGLE_PAGE_FLUSH_CEILING: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TlbRangeFlushMode {
    Pages,
    Full,
}

fn tlb_range_flush_mode(size: usize) -> TlbRangeFlushMode {
    if size.div_ceil(PAGE_SIZE_4K) > TLB_SINGLE_PAGE_FLUSH_CEILING {
        TlbRangeFlushMode::Full
    } else {
        TlbRangeFlushMode::Pages
    }
}

/// Failure while synchronously invalidating a kernel TLB range.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TlbShootdownError {
    /// The target CPU is offline.
    #[error("target CPU is offline")]
    CpuOffline,
    /// The synchronous cross-CPU call timed out.
    #[error("cross-CPU TLB shootdown timed out")]
    Timeout,
    /// This configuration has no cross-CPU invalidation backend.
    #[error("cross-CPU TLB shootdown is not supported")]
    Unsupported,
    /// The platform rejected the cross-CPU operation.
    #[error("platform rejected the cross-CPU TLB shootdown")]
    Platform,
}

/// Flushes the TLB entries covering a virtual-address range on the current CPU.
pub fn flush_tlb_range(start: VirtAddr, size: usize) {
    if size == 0 {
        return;
    }
    if tlb_range_flush_mode(size) == TlbRangeFlushMode::Full {
        ax_cpu::asm::flush_tlb(None);
        return;
    }
    for offset in (0..size).step_by(PAGE_SIZE_4K) {
        ax_cpu::asm::flush_tlb(Some(start + offset));
    }
}

fn update_mmu_cache_with(vaddr: VirtAddr, update: impl FnOnce(VirtAddr)) {
    update(vaddr.align_down_4k());
}

/// Synchronizes a page-table update performed by the local page-fault handler.
///
/// This is the architecture boundary corresponding to Linux's
/// `update_mmu_cache()`: it is intentionally local and must not be replaced by
/// a cross-CPU shootdown. Architectures that do not cache invalid translations
/// implement it as a no-op.
#[inline]
pub fn update_mmu_cache(vaddr: VirtAddr) {
    update_mmu_cache_with(vaddr, ax_cpu::asm::update_mmu_cache);
}

/// Flushes the TLB entries covering a virtual-address range on all available CPUs.
pub fn flush_tlb_range_all_cpus(start: VirtAddr, size: usize) -> Result<(), TlbShootdownError> {
    #[cfg(feature = "ipi")]
    let _guard = ax_sync::PreemptGuard::new();
    let cpu_count = crate::cpu_num().min(usize::BITS as usize);
    let cpu_mask = if cpu_count == usize::BITS as usize {
        usize::MAX
    } else {
        (1usize << cpu_count) - 1
    };
    flush_tlb_range_on_cpus_with(&AxHalTlbShootdown, cpu_mask, start, size)
}

/// Flushes a TLB range on the CPUs selected by `cpu_mask`.
///
/// Bit `n` targets logical CPU `n`. Offline CPUs are skipped because CPU
/// teardown installs the offline root before withdrawing their online state.
pub fn flush_tlb_range_on_cpus(
    cpu_mask: usize,
    start: VirtAddr,
    size: usize,
) -> Result<(), TlbShootdownError> {
    #[cfg(feature = "ipi")]
    let _guard = ax_sync::PreemptGuard::new();
    flush_tlb_range_on_cpus_with(&AxHalTlbShootdown, cpu_mask, start, size)
}

trait TlbShootdown {
    fn cpu_count(&self) -> usize;
    fn current_cpu(&self) -> usize;
    fn cpu_online(&self, cpu_id: usize) -> bool;
    fn flush_remote(
        &self,
        cpu_id: usize,
        start: VirtAddr,
        size: usize,
    ) -> Result<(), TlbShootdownError>;
    fn flush_local(&self, start: VirtAddr, size: usize);
}

struct AxHalTlbShootdown;

impl TlbShootdown for AxHalTlbShootdown {
    fn cpu_count(&self) -> usize {
        crate::cpu_num()
    }

    fn current_cpu(&self) -> usize {
        crate::percpu::this_cpu_id()
    }

    fn cpu_online(&self, cpu_id: usize) -> bool {
        crate::irq::is_cpu_online(cpu_id)
    }

    fn flush_remote(
        &self,
        cpu_id: usize,
        start: VirtAddr,
        size: usize,
    ) -> Result<(), TlbShootdownError> {
        #[cfg(feature = "ipi")]
        {
            let arg = FlushRangeArg {
                start: start.as_usize(),
                size,
            };
            let arg_ptr = &arg as *const FlushRangeArg as *mut ();
            unsafe {
                crate::irq::run_on_cpu_sync(
                    crate::irq::CpuId(cpu_id),
                    flush_tlb_range_thunk,
                    arg_ptr,
                )
            }
            .map_err(|err| match err {
                crate::irq::IrqError::CpuOffline => TlbShootdownError::CpuOffline,
                crate::irq::IrqError::Timeout => TlbShootdownError::Timeout,
                crate::irq::IrqError::Unsupported => TlbShootdownError::Unsupported,
                _ => TlbShootdownError::Platform,
            })
        }
        #[cfg(not(feature = "ipi"))]
        {
            let _ = (cpu_id, start, size);
            Err(TlbShootdownError::Unsupported)
        }
    }

    fn flush_local(&self, start: VirtAddr, size: usize) {
        flush_tlb_range(start, size);
    }
}

fn flush_tlb_range_on_cpus_with(
    runtime: &impl TlbShootdown,
    cpu_mask: usize,
    start: VirtAddr,
    size: usize,
) -> Result<(), TlbShootdownError> {
    let current_cpu = runtime.current_cpu();
    for cpu_id in 0..runtime.cpu_count() {
        let selected = cpu_id < usize::BITS as usize && cpu_mask & (1usize << cpu_id) != 0;
        if !selected || cpu_id == current_cpu || !runtime.cpu_online(cpu_id) {
            continue;
        }
        runtime.flush_remote(cpu_id, start, size)?;
    }
    if current_cpu < usize::BITS as usize && cpu_mask & (1usize << current_cpu) != 0 {
        runtime.flush_local(start, size);
    }
    Ok(())
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
        let _guard = ax_sync::PreemptGuard::new();
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

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    #[test]
    fn local_mmu_cache_update_aligns_the_fault_address_once() {
        let calls = Cell::new(0);
        let observed = Cell::new(VirtAddr::from(0));

        update_mmu_cache_with(VirtAddr::from(0x4567), |vaddr| {
            calls.set(calls.get() + 1);
            observed.set(vaddr);
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(observed.get(), VirtAddr::from(0x4000));
    }

    struct ModelShootdown {
        online: [bool; 3],
        remote_error: Option<TlbShootdownError>,
        remote_cpu: Cell<Option<usize>>,
        local_flushed: Cell<bool>,
    }

    impl TlbShootdown for ModelShootdown {
        fn cpu_count(&self) -> usize {
            self.online.len()
        }

        fn current_cpu(&self) -> usize {
            0
        }

        fn cpu_online(&self, cpu_id: usize) -> bool {
            self.online[cpu_id]
        }

        fn flush_remote(
            &self,
            cpu_id: usize,
            _start: VirtAddr,
            _size: usize,
        ) -> Result<(), TlbShootdownError> {
            self.remote_cpu.set(Some(cpu_id));
            self.remote_error.map_or(Ok(()), Err)
        }

        fn flush_local(&self, _start: VirtAddr, _size: usize) {
            self.local_flushed.set(true);
        }
    }

    #[test]
    fn all_cpu_tlb_shootdown_propagates_remote_failure() {
        let runtime = ModelShootdown {
            online: [true; 3],
            remote_error: Some(TlbShootdownError::Timeout),
            remote_cpu: Cell::new(None),
            local_flushed: Cell::new(false),
        };

        let result =
            flush_tlb_range_on_cpus_with(&runtime, usize::MAX, VirtAddr::from(0x4000), 0x2000);

        assert_eq!(result, Err(TlbShootdownError::Timeout));
        assert_eq!(runtime.remote_cpu.get(), Some(1));
        assert!(!runtime.local_flushed.get());
    }

    #[test]
    fn all_cpu_tlb_shootdown_skips_offline_cpus_then_flushes_local() {
        let runtime = ModelShootdown {
            online: [true, false, true],
            remote_error: None,
            remote_cpu: Cell::new(None),
            local_flushed: Cell::new(false),
        };

        let result =
            flush_tlb_range_on_cpus_with(&runtime, usize::MAX, VirtAddr::from(0x4000), 0x2000);

        assert_eq!(result, Ok(()));
        assert_eq!(runtime.remote_cpu.get(), Some(2));
        assert!(runtime.local_flushed.get());
    }

    #[test]
    fn targeted_tlb_shootdown_skips_unselected_remote_and_local_cpus() {
        let runtime = ModelShootdown {
            online: [true; 3],
            remote_error: None,
            remote_cpu: Cell::new(None),
            local_flushed: Cell::new(false),
        };

        let result =
            flush_tlb_range_on_cpus_with(&runtime, 1usize << 2, VirtAddr::from(0x4000), 0x2000);

        assert_eq!(result, Ok(()));
        assert_eq!(runtime.remote_cpu.get(), Some(2));
        assert!(!runtime.local_flushed.get());
    }

    #[test]
    fn large_tlb_ranges_switch_to_one_full_invalidation() {
        assert_eq!(tlb_range_flush_mode(0), TlbRangeFlushMode::Pages);
        assert_eq!(
            tlb_range_flush_mode(TLB_SINGLE_PAGE_FLUSH_CEILING * PAGE_SIZE_4K),
            TlbRangeFlushMode::Pages
        );
        assert_eq!(
            tlb_range_flush_mode((TLB_SINGLE_PAGE_FLUSH_CEILING + 1) * PAGE_SIZE_4K),
            TlbRangeFlushMode::Full
        );
    }
}
