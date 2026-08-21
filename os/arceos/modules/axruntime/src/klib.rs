//! Platform implementation of the `axklib::Klib` trait.
//!
//! This crate provides the platform-side glue that implements the small set
//! of kernel helper functions defined in `axklib`. The implementation is
//! intentionally minimal: it forwards memory mapping requests to `axmm`,
//! delegates timing to `ax-hal`, and wires IRQ operations to `ax-hal` when the
//! `irq` feature is enabled.
//!
//! The implementation uses the `impl_trait!` helper to generate the FFI
//! shims expected by consumers. Documentation here focuses on the behavior
//! and expectations of each exported function.

use core::{ptr::NonNull, time::Duration};

#[cfg(feature = "paging")]
use ax_memory_addr::MemoryAddr;
use axklib::{
    BoxedIrqHandler, ConcurrentBoxedIrqHandler, DmaCoherentMappingOutcome, IrqCpuId, IrqCpuMask,
    IrqError, IrqHandle, IrqId, Klib, KlibError, KlibResult, PhysAddr, VirtAddr, impl_trait,
};

struct KlibImpl;

#[cfg(feature = "paging")]
pub(crate) fn map_mm_error(err: ax_mm::MmError) -> KlibError {
    match err {
        ax_mm::MmError::InvalidInput(_) => KlibError::InvalidInput,
        ax_mm::MmError::NoMemory => KlibError::NoMemory,
        ax_mm::MmError::AlreadyExists => KlibError::AlreadyExists,
        ax_mm::MmError::BadAddress => KlibError::BadAddress,
        ax_mm::MmError::BadState(_) => KlibError::BadState,
        ax_mm::MmError::Unsupported => KlibError::Unsupported,
    }
}

#[cfg(feature = "paging")]
fn dma_coherent_range(addr: NonNull<u8>, size: usize) -> Option<(VirtAddr, usize)> {
    if size == 0 {
        return None;
    }

    let addr = VirtAddr::from_usize(addr.as_ptr() as usize);
    let start = addr.align_down_4k();
    let end = (addr + size).align_up_4k();
    Some((start, end - start))
}

#[cfg(feature = "irq")]
fn map_irq_error(err: IrqError) -> KlibError {
    match err {
        IrqError::InvalidIrq | IrqError::InvalidCpu => KlibError::InvalidInput,
        IrqError::CpuOffline | IrqError::Unsupported => KlibError::Unsupported,
        IrqError::Timeout => KlibError::TimedOut,
        IrqError::Busy | IrqError::InIrqContext => KlibError::ResourceBusy,
        IrqError::NoMemory => KlibError::NoMemory,
        IrqError::NotFound => KlibError::NotFound,
        IrqError::Controller => KlibError::Io,
    }
}

fn dma_cache_range(op: ax_hal::mem::DCacheOp, addr: VirtAddr, size: usize) {
    ax_hal::mem::dcache_range(op, addr, size);
}

fn validate_dma_allocation(
    addr: usize,
    num_pages: usize,
    dealloc: impl FnOnce(usize, usize),
) -> KlibResult<NonNull<u8>> {
    if num_pages == 0 {
        return Ok(NonNull::dangling());
    }
    if let Some(ptr) = NonNull::new(addr as *mut u8) {
        return Ok(ptr);
    }

    dealloc(addr, num_pages);
    Err(KlibError::BadState)
}

impl_trait! {
    impl Klib for KlibImpl {
        /// Map a physical region by delegating to the memory manager (`axmm`).
        ///
        /// This function forwards the request to `ax_mm::iomap` and returns the
        /// resulting virtual address wrapped in a `KlibResult`.
        fn mem_iomap(addr: PhysAddr, size: usize) -> KlibResult<VirtAddr> {
            #[cfg(feature = "paging")]
            {
                ax_mm::iomap(addr, size).map_err(map_mm_error)
            }
            #[cfg(not(feature = "paging"))]
            {
                let _ = (addr, size);
                Err(KlibError::Unsupported)
            }
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            ax_hal::mem::virt_to_phys(addr)
        }

        fn dma_cache_clean(addr: VirtAddr, size: usize) {
            dma_cache_range(ax_hal::mem::DCacheOp::Clean, addr, size);
        }

        fn dma_cache_invalidate(addr: VirtAddr, size: usize) {
            dma_cache_range(ax_hal::mem::DCacheOp::Invalidate, addr, size);
        }

        fn dma_cache_clean_invalidate(addr: VirtAddr, size: usize) {
            dma_cache_range(ax_hal::mem::DCacheOp::CleanInvalidate, addr, size);
        }

        fn mem_map_dma_coherent_uncached(
            addr: NonNull<u8>,
            size: usize,
        ) -> DmaCoherentMappingOutcome {
            #[cfg(feature = "paging")]
            {
                let Some((start, size)) = dma_coherent_range(addr, size) else {
                    return DmaCoherentMappingOutcome::Mapped(addr);
                };

                ax_hal::mem::dma_coherent_before_map_uncached(start, size);
                let paddr = ax_hal::mem::virt_to_phys(start);
                let alias = match crate::kernel_mapping::map_dma_coherent_alias(paddr, size) {
                    Ok(alias) => alias,
                    Err(crate::kernel_mapping::MappingTransactionError::NotStarted(err)) => {
                        return DmaCoherentMappingOutcome::NotStarted(
                            crate::error::runtime_error_to_klib_error(err),
                        );
                    }
                    Err(crate::kernel_mapping::MappingTransactionError::StateUncertain(err)) => {
                        return DmaCoherentMappingOutcome::StateUncertain(
                            crate::error::runtime_error_to_klib_error(err),
                        );
                    }
                };
                ax_hal::mem::dma_coherent_after_mapping_update();
                DmaCoherentMappingOutcome::Mapped(alias)
            }
            #[cfg(not(feature = "paging"))]
            {
                let _ = (addr, size);
                DmaCoherentMappingOutcome::NotStarted(KlibError::Unsupported)
            }
        }

        fn mem_unmap_dma_coherent(addr: NonNull<u8>, size: usize) -> KlibResult {
            #[cfg(feature = "paging")]
            {
                let Some((start, size)) = dma_coherent_range(addr, size) else {
                    return Ok(());
                };

                ax_hal::mem::dma_coherent_before_unmap_uncached(start, size);
                crate::kernel_mapping::unmap_dma_coherent_alias(addr, size)
                .map_err(crate::error::runtime_error_to_klib_error)?;
                ax_hal::mem::dma_coherent_after_mapping_update();
                Ok(())
            }
            #[cfg(not(feature = "paging"))]
            {
                let _ = (addr, size);
                Err(KlibError::Unsupported)
            }
        }

        fn dma_alloc_pages(
            dma_mask: u64,
            num_pages: usize,
            align: usize,
        ) -> KlibResult<NonNull<u8>> {
            if num_pages == 0 {
                return Ok(NonNull::dangling());
            }
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
            }
            .map_err(|_| KlibError::NoMemory)?;
            validate_dma_allocation(addr, num_pages, |addr, num_pages| {
                ax_alloc::global_allocator().dealloc_pages(
                    addr,
                    num_pages,
                    ax_alloc::UsageKind::Dma,
                );
            })
        }

        fn dma_dealloc_pages(addr: NonNull<u8>, num_pages: usize) {
            if num_pages == 0 {
                return;
            }
            ax_alloc::global_allocator().dealloc_pages(
                addr.as_ptr() as usize,
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
        ///
        /// When the `irq` feature is enabled this forwards to
        /// `ax_hal::irq::set_enable`. Platforms built without IRQ support
        /// ignore this request because there is no interrupt controller
        /// service to program.
        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> KlibResult {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::set_enable(_irq, _enabled).map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(KlibError::Unsupported)
            }
        }

        fn irq_request_shared(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::request_shared_irq(_irq, _handler).map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(KlibError::Unsupported)
            }
        }

        fn irq_request_shared_disabled(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::request_irq(
                    _irq,
                    ax_hal::irq::IrqRequest::new(_handler)
                        .share_mode(ax_hal::irq::ShareMode::Shared)
                        .auto_enable(ax_hal::irq::AutoEnable::No),
                )
                .map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(KlibError::Unsupported)
            }
        }

        fn irq_request_percpu(
            _irq: IrqId,
            _cpus: IrqCpuMask,
            _handler: ConcurrentBoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::request_percpu_irq(_irq, _cpus, _handler)
                    .map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(KlibError::Unsupported)
            }
        }

        fn irq_free(_handle: IrqHandle) -> KlibResult {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::free_irq(_handle).map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(KlibError::Unsupported)
            }
        }

        fn irq_enable(_handle: IrqHandle) -> KlibResult {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::enable_irq(_handle).map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(KlibError::Unsupported)
            }
        }

        fn irq_disable(_handle: IrqHandle) -> KlibResult {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::disable_irq(_handle).map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(KlibError::Unsupported)
            }
        }

        unsafe fn irq_run_on_cpu_sync(
            _cpu: IrqCpuId,
            _f: unsafe fn(*mut ()),
            _arg: *mut (),
        ) -> Result<(), IrqError> {
            #[cfg(feature = "irq")]
            {
                unsafe { ax_hal::irq::run_on_cpu_sync(_cpu, _f, _arg) }
            }
            #[cfg(not(feature = "irq"))]
            {
                let _ = (_cpu, _f, _arg);
                Err(IrqError::Unsupported)
            }
        }
    }
}

#[cfg(all(test, not(feature = "paging")))]
mod tests {
    use super::*;

    #[test]
    fn coherent_mapping_reports_not_started_without_paging() {
        assert_eq!(
            KlibImpl::mem_map_dma_coherent_uncached(
                NonNull::new(0x1000 as *mut u8).unwrap(),
                0x1000,
            ),
            DmaCoherentMappingOutcome::NotStarted(KlibError::Unsupported)
        );
    }

    #[test]
    fn null_dma_page_allocation_is_reclaimed_before_error() {
        let mut released = None;

        let result = validate_dma_allocation(0, 3, |addr, num_pages| {
            released = Some((addr, num_pages));
        });

        assert_eq!(result, Err(KlibError::BadState));
        assert_eq!(released, Some((0, 3)));
    }
}
