//! Platform implementation of the `axklib::Klib` trait for ArceBoot.
//!
//! ArceBoot has paging and an allocator but no IRQ handling, so the memory and
//! time services forward to `ax-mm`/`ax-alloc`/`ax-hal` while every IRQ
//! service reports `Unsupported`. This mirrors the `axruntime` glue
//! (`axruntime/src/klib.rs`) minus the task/IRQ plumbing.

use core::{ptr::NonNull, time::Duration};

use axklib::{
    BoxedIrqHandler, ConcurrentBoxedIrqHandler, DmaCoherentMappingOutcome, IrqCpuId, IrqCpuMask,
    IrqError, IrqHandle, IrqId, Klib, KlibError, KlibResult, PhysAddr, VirtAddr, impl_trait,
};

struct KlibImpl;

fn map_mm_error(err: ax_mm::MmError) -> KlibError {
    match err {
        ax_mm::MmError::InvalidInput(_) => KlibError::InvalidInput,
        ax_mm::MmError::NoMemory => KlibError::NoMemory,
        ax_mm::MmError::AlreadyExists => KlibError::AlreadyExists,
        ax_mm::MmError::BadAddress => KlibError::BadAddress,
        ax_mm::MmError::BadState(_) => KlibError::BadState,
        ax_mm::MmError::Unsupported => KlibError::Unsupported,
    }
}

impl_trait! {
    impl Klib for KlibImpl {
        /// Map a physical region by delegating to the memory manager (`axmm`).
        fn mem_iomap(addr: PhysAddr, size: usize) -> KlibResult<VirtAddr> {
            ax_mm::iomap(addr, size).map_err(map_mm_error)
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            ax_hal::mem::virt_to_phys(addr)
        }

        fn dma_cache_clean(addr: VirtAddr, size: usize) {
            ax_hal::mem::dcache_range(ax_hal::mem::DCacheOp::Clean, addr, size);
        }

        fn dma_cache_invalidate(addr: VirtAddr, size: usize) {
            ax_hal::mem::dcache_range(ax_hal::mem::DCacheOp::Invalidate, addr, size);
        }

        fn dma_cache_clean_invalidate(addr: VirtAddr, size: usize) {
            ax_hal::mem::dcache_range(ax_hal::mem::DCacheOp::CleanInvalidate, addr, size);
        }

        fn mem_map_dma_coherent_uncached(
            addr: NonNull<u8>,
            size: usize,
        ) -> DmaCoherentMappingOutcome {
            let _ = (addr, size);
            DmaCoherentMappingOutcome::NotStarted(KlibError::Unsupported)
        }

        fn mem_unmap_dma_coherent(addr: NonNull<u8>, size: usize) -> KlibResult {
            let _ = (addr, size);
            Err(KlibError::Unsupported)
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
            NonNull::new(addr as *mut u8).ok_or(KlibError::BadState)
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

        fn time_busy_wait(dur: Duration) {
            ax_hal::time::busy_wait(dur);
        }

        fn time_monotonic_nanos() -> u64 {
            ax_hal::time::monotonic_time_nanos()
        }

        fn time_try_init_epoch_offset(epoch_time_nanos: u64) -> bool {
            ax_hal::time::try_init_epoch_offset(epoch_time_nanos)
        }

        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn irq_request_shared(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_request_shared_disabled(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_request_percpu(
            _irq: IrqId,
            _cpus: IrqCpuMask,
            _handler: ConcurrentBoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_free(_handle: IrqHandle) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn irq_enable(_handle: IrqHandle) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn irq_disable(_handle: IrqHandle) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        unsafe fn irq_run_on_cpu_sync(
            _cpu: IrqCpuId,
            _f: unsafe fn(*mut ()),
            _arg: *mut (),
        ) -> Result<(), IrqError> {
            Err(IrqError::Unsupported)
        }
    }
}
