//! Single trait-FFI provider used by the ax-driver library test binary.

#[cfg(feature = "virtio-blk")]
extern crate std;

#[cfg(feature = "virtio-blk")]
use core::{
    alloc::{GlobalAlloc, Layout},
    cell::Cell,
};
#[cfg(feature = "virtio-blk")]
use std::alloc::System;

use axklib::{
    AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle, IrqId,
    Klib, PhysAddr, VirtAddr, impl_trait,
};

struct TestKlib;

#[cfg(feature = "virtio-blk")]
struct AuditAllocator;

#[cfg(feature = "virtio-blk")]
std::thread_local! {
    static AUDIT_ENABLED: Cell<bool> = const { Cell::new(false) };
    static AUDIT_ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static AUDIT_DEALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every operation delegates to `System` with the original pointer and
// layout. Thread-local counters are observational and exclude unrelated test
// threads from deterministic hot-path allocation audits.
#[cfg(feature = "virtio-blk")]
unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && allocation_audit_enabled() {
            AUDIT_ALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() && allocation_audit_enabled() {
            AUDIT_ALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if allocation_audit_enabled() {
            AUDIT_DEALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, new_size) };
        if !replacement.is_null() && allocation_audit_enabled() {
            AUDIT_ALLOCATIONS.with(|count| count.set(count.get() + 1));
            AUDIT_DEALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
        replacement
    }
}

#[global_allocator]
#[cfg(feature = "virtio-blk")]
static GLOBAL_ALLOCATOR: AuditAllocator = AuditAllocator;

#[cfg(feature = "virtio-blk")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AllocationActivity {
    pub(crate) allocations: usize,
    pub(crate) deallocations: usize,
}

#[cfg(feature = "virtio-blk")]
pub(crate) fn audit_allocations<R>(operation: impl FnOnce() -> R) -> (R, AllocationActivity) {
    AUDIT_ALLOCATIONS.with(|count| count.set(0));
    AUDIT_DEALLOCATIONS.with(|count| count.set(0));
    AUDIT_ENABLED.with(|enabled| enabled.set(true));
    let result = operation();
    AUDIT_ENABLED.with(|enabled| enabled.set(false));
    let activity = AllocationActivity {
        allocations: AUDIT_ALLOCATIONS.with(Cell::get),
        deallocations: AUDIT_DEALLOCATIONS.with(Cell::get),
    };
    (result, activity)
}

#[cfg(feature = "virtio-blk")]
fn allocation_audit_enabled() -> bool {
    AUDIT_ENABLED.try_with(Cell::get).unwrap_or(false)
}

impl_trait! {
    impl Klib for TestKlib {
        fn mem_iomap(_addr: PhysAddr, _size: usize) -> AxResult<VirtAddr> {
            Err(AxError::Unsupported)
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            PhysAddr::from_usize(addr.as_usize())
        }

        fn mem_make_dma_coherent_uncached(_addr: VirtAddr, _size: usize) -> AxResult {
            Err(AxError::Unsupported)
        }

        fn mem_restore_dma_cached(_addr: VirtAddr, _size: usize) -> AxResult {
            Err(AxError::Unsupported)
        }

        // Host unit tests use ordinary coherent process memory for their DMA
        // model. Keep these explicit no-ops in the test provider so every
        // trait-FFI symbol exists without changing the production ABI.
        fn dma_cache_clean(_addr: VirtAddr, _size: usize) {}

        fn dma_cache_invalidate(_addr: VirtAddr, _size: usize) {}

        fn dma_cache_clean_invalidate(_addr: VirtAddr, _size: usize) {}

        fn dma_alloc_pages(
            _dma_mask: u64,
            _num_pages: usize,
            _align: usize,
        ) -> AxResult<VirtAddr> {
            Err(AxError::Unsupported)
        }

        fn dma_dealloc_pages(_addr: VirtAddr, _num_pages: usize) {}

        fn time_busy_wait(_dur: core::time::Duration) {}

        fn time_monotonic_nanos() -> u64 {
            0
        }

        fn time_try_init_epoch_offset(_epoch_time_nanos: u64) -> bool {
            false
        }

        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> AxResult {
            Ok(())
        }

        fn irq_request_shared(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
            Err(AxError::Unsupported)
        }

        fn irq_request_shared_disabled(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
            Err(AxError::Unsupported)
        }

        fn irq_request_percpu(
            _irq: IrqId,
            _cpus: IrqCpuMask,
            _handler: ConcurrentBoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
            Err(AxError::Unsupported)
        }

        fn irq_free(_handle: IrqHandle) -> AxResult {
            Err(AxError::Unsupported)
        }

        fn irq_enable(_handle: IrqHandle) -> AxResult {
            Err(AxError::Unsupported)
        }

        fn irq_disable(_handle: IrqHandle) -> AxResult {
            Err(AxError::Unsupported)
        }
    }
}
