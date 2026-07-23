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

use core::time::Duration;

#[cfg(feature = "paging")]
use ax_memory_addr::MemoryAddr;
use axklib::{
    AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuId, IrqCpuMask, IrqError,
    IrqHandle, IrqId, Klib, PhysAddr, VirtAddr,
    dma::{DmaPageAllocation, DmaPageZone},
    impl_trait,
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

#[cfg(feature = "irq")]
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

fn dma_cache_range(op: ax_hal::mem::DCacheOp, addr: VirtAddr, size: usize) {
    ax_hal::mem::dcache_range(op, addr, size);
}

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

        fn dma_cache_clean(addr: VirtAddr, size: usize) {
            dma_cache_range(ax_hal::mem::DCacheOp::Clean, addr, size);
        }

        fn dma_cache_invalidate(addr: VirtAddr, size: usize) {
            dma_cache_range(ax_hal::mem::DCacheOp::Invalidate, addr, size);
        }

        fn dma_cache_clean_invalidate(addr: VirtAddr, size: usize) {
            dma_cache_range(ax_hal::mem::DCacheOp::CleanInvalidate, addr, size);
        }

        fn mem_make_dma_coherent_uncached(addr: VirtAddr, size: usize) -> AxResult {
            #[cfg(feature = "paging")]
            {
                let Some((start, size)) = dma_coherent_range(addr, size) else {
                    return Ok(());
                };

                ax_hal::mem::dma_coherent_before_make_uncached(start, size);
                ax_mm::kernel_aspace().lock().protect(
                    start,
                    size,
                    ax_hal::paging::MappingFlags::READ
                        | ax_hal::paging::MappingFlags::WRITE
                        | ax_hal::paging::MappingFlags::UNCACHED,
                )?;
                ax_hal::asm::flush_tlb(None);
                ax_hal::mem::dma_coherent_after_mapping_update();
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

                ax_hal::mem::dma_coherent_before_restore_cached(start, size);
                ax_mm::kernel_aspace().lock().protect(
                    start,
                    size,
                    ax_hal::paging::MappingFlags::READ | ax_hal::paging::MappingFlags::WRITE,
                )?;
                ax_hal::asm::flush_tlb(None);
                ax_hal::mem::dma_coherent_after_mapping_update();
                Ok(())
            }
            #[cfg(not(feature = "paging"))]
            {
                let _ = (addr, size);
                Err(AxError::Unsupported)
            }
        }

        fn dma_alloc_pages(
            dma_mask: u64,
            num_pages: usize,
            align: usize,
        ) -> AxResult<DmaPageAllocation> {
            let (dma_zone, allocator_zone) = if dma_mask <= u32::MAX as u64 {
                (DmaPageZone::Dma32, ax_alloc::MemoryZone::Dma32)
            } else {
                (DmaPageZone::Normal, ax_alloc::MemoryZone::Normal)
            };
            let addr = ax_alloc::global_allocator().allocate_pages_raw(
                ax_alloc::PageRequest {
                    count: num_pages,
                    align,
                    zone: allocator_zone,
                },
                ax_alloc::UsageKind::Dma,
            )?;
            Ok(DmaPageAllocation::new(
                VirtAddr::from(addr),
                num_pages,
                dma_zone,
            ))
        }

        fn dma_dealloc_pages(allocation: DmaPageAllocation) {
            let (addr, num_pages, zone) = allocation.into_parts();
            // SAFETY: consuming DmaPageAllocation proves unique ownership and
            // returns the unchanged address, count, and source zone.
            unsafe {
                ax_alloc::global_allocator().deallocate_pages_raw(
                    addr.as_usize(),
                    ax_alloc::PageRequest {
                        count: num_pages,
                        align: ax_memory_addr::PAGE_SIZE_4K,
                        zone: match zone {
                            DmaPageZone::Normal => ax_alloc::MemoryZone::Normal,
                            DmaPageZone::Dma32 => ax_alloc::MemoryZone::Dma32,
                        },
                    },
                    ax_alloc::UsageKind::Dma,
                );
            }
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
        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> AxResult {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::set_enable(_irq, _enabled).map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(AxError::Unsupported)
            }
        }

        fn irq_request_shared(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::request_shared_irq(_irq, _handler).map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(AxError::Unsupported)
            }
        }

        fn irq_request_shared_disabled(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
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
                Err(AxError::Unsupported)
            }
        }

        fn irq_request_percpu(
            _irq: IrqId,
            _cpus: IrqCpuMask,
            _handler: ConcurrentBoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::request_percpu_irq(_irq, _cpus, _handler)
                    .map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(AxError::Unsupported)
            }
        }

        fn irq_free(_handle: IrqHandle) -> AxResult {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::free_irq(_handle).map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(AxError::Unsupported)
            }
        }

        fn irq_enable(_handle: IrqHandle) -> AxResult {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::enable_irq(_handle).map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(AxError::Unsupported)
            }
        }

        fn irq_disable(_handle: IrqHandle) -> AxResult {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::disable_irq(_handle).map_err(map_irq_error)
            }
            #[cfg(not(feature = "irq"))]
            {
                Err(AxError::Unsupported)
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
