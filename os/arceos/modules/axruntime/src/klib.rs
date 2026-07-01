//! Platform implementation of the `axklib::Klib` trait.
//!
//! This crate provides the platform-side glue that implements the small set
//! of kernel helper functions defined in `axklib`. The implementation is
//! intentionally minimal: it forwards memory mapping requests to `axmm`,
//! delegates timing to `ax-hal`, and wires IRQ operations to `ax-hal`.
//!
//! The implementation uses the `impl_trait!` helper to generate the FFI
//! shims expected by consumers. Documentation here focuses on the behavior
//! and expectations of each exported function.

use core::time::Duration;

#[cfg(feature = "paging")]
use ax_memory_addr::MemoryAddr;
use axklib::{
    AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuId, IrqCpuMask, IrqError,
    IrqHandle, IrqId, Klib, PhysAddr, VirtAddr, impl_trait,
};

struct KlibImpl;

#[cfg(feature = "paging")]
fn dma_coherent_range(addr: VirtAddr, size: usize) -> Option<(VirtAddr, usize)> {
    if size == 0 {
        return None;
    }

    let start = addr.align_down_4k();
    let end = (addr + size).align_up_4k();
    Some((start, end - start))
}

fn map_irq_error(err: IrqError) -> AxError {
    match err {
        IrqError::InvalidIrq | IrqError::InvalidCpu => AxError::InvalidInput,
        IrqError::CpuOffline | IrqError::Unsupported => AxError::Unsupported,
        IrqError::Timeout => AxError::TimedOut,
        IrqError::Busy | IrqError::InIrqContext => AxError::ResourceBusy,
        IrqError::NoMemory => AxError::NoMemory,
        IrqError::NotFound => AxError::NotFound,
        IrqError::Controller => AxError::Io,
    }
}

#[cfg(all(feature = "paging", target_arch = "aarch64"))]
fn clean_invalidate_dcache_to_poc(addr: VirtAddr, size: usize) {
    use core::arch::asm;

    if size == 0 {
        return;
    }

    let line_size = ax_hal::asm::dcache_line_size_from_ctr();
    let start = addr.as_usize() & !(line_size - 1);
    let end = (addr.as_usize() + size + line_size - 1) & !(line_size - 1);
    for line in (start..end).step_by(line_size) {
        unsafe { asm!("dc civac, {0:x}", in(reg) line) };
    }
}

#[cfg(all(feature = "paging", not(target_arch = "aarch64")))]
fn clean_invalidate_dcache_to_poc(_addr: VirtAddr, _size: usize) {}

#[cfg(all(feature = "paging", target_arch = "aarch64"))]
#[inline]
fn dsb_sy() {
    unsafe { core::arch::asm!("dsb sy") };
}

#[cfg(all(feature = "paging", not(target_arch = "aarch64")))]
#[inline]
fn dsb_sy() {}

#[cfg(all(feature = "paging", target_arch = "aarch64"))]
#[inline]
fn isb_sy() {
    unsafe { core::arch::asm!("isb") };
}

#[cfg(all(feature = "paging", not(target_arch = "aarch64")))]
#[inline]
fn isb_sy() {}

impl_trait! {
    impl Klib for KlibImpl {
        /// Map a physical region by delegating to the memory manager (`axmm`).
        ///
        /// This function forwards the request to `ax_mm::iomap` and returns the
        /// resulting virtual address wrapped in an `AxResult`.
        fn mem_iomap(addr: PhysAddr, size: usize) -> AxResult<VirtAddr> {
            #[cfg(feature = "paging")]
            {
                // Convert from AxError (struct in ax_errno 0.2) to AxErrorKind (enum used by axklib)
                ax_mm::iomap(addr, size)
            }
            #[cfg(not(feature = "paging"))]
            {
                let _ = (addr, size);
                Err(AxError::Unsupported)
            }
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            ax_hal::mem::virt_to_phys(addr)
        }

        fn mem_make_dma_coherent_uncached(addr: VirtAddr, size: usize) -> AxResult {
            #[cfg(feature = "paging")]
            {
                let Some((start, size)) = dma_coherent_range(addr, size) else {
                    return Ok(());
                };

                clean_invalidate_dcache_to_poc(start, size);
                dsb_sy();
                ax_mm::kernel_aspace().lock().protect(
                    start,
                    size,
                    ax_hal::paging::MappingFlags::READ
                        | ax_hal::paging::MappingFlags::WRITE
                        | ax_hal::paging::MappingFlags::UNCACHED,
                )?;
                ax_hal::asm::flush_tlb(None);
                dsb_sy();
                isb_sy();
                Ok(())
            }
            #[cfg(not(feature = "paging"))]
            {
                let _ = (addr, size);
                Err(AxError::Unsupported)
            }
        }

        fn mem_restore_dma_cached(addr: VirtAddr, size: usize) -> AxResult {
            #[cfg(feature = "paging")]
            {
                let Some((start, size)) = dma_coherent_range(addr, size) else {
                    return Ok(());
                };

                dsb_sy();
                ax_mm::kernel_aspace().lock().protect(
                    start,
                    size,
                    ax_hal::paging::MappingFlags::READ | ax_hal::paging::MappingFlags::WRITE,
                )?;
                ax_hal::asm::flush_tlb(None);
                dsb_sy();
                isb_sy();
                Ok(())
            }
            #[cfg(not(feature = "paging"))]
            {
                let _ = (addr, size);
                Err(AxError::Unsupported)
            }
        }

        fn dma_alloc_pages(dma_mask: u64, num_pages: usize, align: usize) -> AxResult<VirtAddr> {
            let addr = if dma_mask <= u32::MAX as u64 {
                ax_alloc::global_allocator().alloc_dma32_pages(
                    num_pages,
                    align,
                    ax_alloc::UsageKind::Dma,
                )
            } else {
                ax_alloc::global_allocator().alloc_pages(
                    num_pages,
                    align,
                    ax_alloc::UsageKind::Dma,
                )
            }?;
            Ok(VirtAddr::from(addr))
        }

        fn dma_dealloc_pages(addr: VirtAddr, num_pages: usize) {
            ax_alloc::global_allocator().dealloc_pages(
                addr.as_usize(),
                num_pages,
                ax_alloc::UsageKind::Dma,
            );
        }

        /// Busy-wait for the given duration by calling into `ax-hal`.
        ///
        /// Short delays are serviced by the hardware abstraction layer's
        /// busy-wait implementation. This is suitable for small spin waits
        /// but should not be used for long sleeps.
        fn time_busy_wait(dur: Duration) {
            ax_hal::time::busy_wait(dur);
        }

        fn time_monotonic_nanos() -> u64 {
            ax_hal::time::monotonic_time_nanos()
        }

        fn time_try_init_epoch_offset(epoch_time_nanos: u64) -> bool {
            ax_hal::time::try_init_epoch_offset(epoch_time_nanos)
        }

        /// Enable or disable the specified IRQ line.
        fn irq_set_enable(irq: IrqId, enabled: bool) -> AxResult {
            ax_hal::irq::set_enable(irq, enabled).map_err(map_irq_error)
        }

        fn irq_request_shared(irq: IrqId, handler: BoxedIrqHandler) -> AxResult<IrqHandle> {
            ax_hal::irq::request_shared_irq(irq, handler).map_err(map_irq_error)
        }

        fn irq_request_shared_disabled(
            irq: IrqId,
            handler: BoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
            ax_hal::irq::request_irq(
                irq,
                ax_hal::irq::IrqRequest::new(handler)
                    .share_mode(ax_hal::irq::ShareMode::Shared)
                    .auto_enable(ax_hal::irq::AutoEnable::No),
            )
            .map_err(map_irq_error)
        }

        fn irq_request_percpu(
            irq: IrqId,
            cpus: IrqCpuMask,
            handler: ConcurrentBoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
            ax_hal::irq::request_percpu_irq(irq, cpus, handler).map_err(map_irq_error)
        }

        fn irq_free(handle: IrqHandle) -> AxResult {
            ax_hal::irq::free_irq(handle).map_err(map_irq_error)
        }

        fn irq_enable(handle: IrqHandle) -> AxResult {
            ax_hal::irq::enable_irq(handle).map_err(map_irq_error)
        }

        fn irq_disable(handle: IrqHandle) -> AxResult {
            ax_hal::irq::disable_irq(handle).map_err(map_irq_error)
        }

        unsafe fn irq_run_on_cpu_sync(
            cpu: IrqCpuId,
            f: unsafe fn(*mut ()),
            arg: *mut (),
        ) -> Result<(), IrqError> {
            unsafe { ax_hal::irq::run_on_cpu_sync(cpu, f, arg) }
        }
    }
}
