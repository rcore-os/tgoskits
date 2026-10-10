//! Host-only lock and kernel helper providers for deterministic driver tests.
//! These do not model IRQ, preemption, scheduling, or hardware behavior.

use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use ax_sync::interface::{AcquireResult, ContextState, LOCK_MODE_READ, LockMetadata};
use axklib::{
    BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle, IrqId, KlibError,
    KlibResult, PhysAddr, VirtAddr, klib::impl_trait,
};

struct HostLocks;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::SpinOps for HostLocks {
    fn acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _addr: usize,
        _context: u8,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> ContextState {
        while locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        ContextState::new(0, 0)
    }
    fn try_acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _addr: usize,
        _context: u8,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        AcquireResult::new(
            locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok(),
            ContextState::new(0, 0),
        )
    }
    fn release(locked: &AtomicBool, _addr: usize, _context: u8, _state: ContextState) {
        locked.store(false, Ordering::Release);
    }
    fn force_release(locked: &AtomicBool, _addr: usize, _context: u8) {
        locked.store(false, Ordering::Release);
    }
    fn is_locked(locked: &AtomicBool) -> bool {
        locked.load(Ordering::Relaxed)
    }
}

const WRITER: usize = 1 << (usize::BITS - 1);

fn try_rwlock(state: &AtomicUsize, mode: u8) -> bool {
    if mode != LOCK_MODE_READ {
        return state
            .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_ok();
    }
    state
        .try_update(Ordering::Acquire, Ordering::Relaxed, |readers| {
            (readers < WRITER - 1).then(|| readers + 1)
        })
        .is_ok()
}

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::RwLockOps for HostLocks {
    fn acquire(
        state: &AtomicUsize,
        _metadata: &LockMetadata,
        _addr: usize,
        _context: u8,
        mode: u8,
        _caller: &'static Location<'static>,
    ) -> ContextState {
        while !try_rwlock(state, mode) {
            core::hint::spin_loop();
        }
        ContextState::new(0, 0)
    }
    fn try_acquire(
        state: &AtomicUsize,
        _metadata: &LockMetadata,
        _addr: usize,
        _context: u8,
        mode: u8,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        AcquireResult::new(try_rwlock(state, mode), ContextState::new(0, 0))
    }
    fn release(
        state: &AtomicUsize,
        _addr: usize,
        _context: u8,
        _context_state: ContextState,
        mode: u8,
    ) {
        if mode == LOCK_MODE_READ {
            state.fetch_sub(1, Ordering::Release);
        } else {
            state.store(0, Ordering::Release);
        }
    }
    fn force_read_decrement(state: &AtomicUsize, _addr: usize, _context: u8) {
        state.fetch_sub(1, Ordering::Release);
    }
}

static MMIO_PADDR: AtomicUsize = AtomicUsize::new(0);
static MMIO_SIZE: AtomicUsize = AtomicUsize::new(0);
static MMIO_VADDR: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "starfive-soc")]
pub fn register_mmio_mapping(paddr: usize, size: usize, vaddr: usize) {
    MMIO_PADDR.store(paddr, Ordering::SeqCst);
    MMIO_SIZE.store(size, Ordering::SeqCst);
    MMIO_VADDR.store(vaddr, Ordering::SeqCst);
}

struct KlibImpl;

impl_trait! {
    impl Klib for KlibImpl {
        fn mem_iomap(addr: PhysAddr, size: usize) -> KlibResult<VirtAddr> {
            let matches = addr.as_usize() == MMIO_PADDR.load(Ordering::SeqCst)
                && size == MMIO_SIZE.load(Ordering::SeqCst);
            let ptr = MMIO_VADDR.load(Ordering::SeqCst);
            if matches && ptr != 0 {
                Ok(VirtAddr::from_usize(ptr))
            } else {
                Err(KlibError::Unsupported)
            }
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            PhysAddr::from_usize(addr.as_usize())
        }

        fn mem_map_dma_coherent_uncached(
            _addr: core::ptr::NonNull<u8>,
            _size: usize,
        ) -> axklib::DmaCoherentMappingOutcome {
            axklib::DmaCoherentMappingOutcome::NotStarted(KlibError::Unsupported)
        }

        fn mem_unmap_dma_coherent(_addr: core::ptr::NonNull<u8>, _size: usize) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn dma_cache_clean(_addr: VirtAddr, _size: usize) {}

        fn dma_cache_invalidate(_addr: VirtAddr, _size: usize) {}

        fn dma_cache_clean_invalidate(_addr: VirtAddr, _size: usize) {}

        fn dma_alloc_pages(_dma_mask: u64, _num_pages: usize, _align: usize) -> KlibResult<core::ptr::NonNull<u8>> {
            Err(KlibError::Unsupported)
        }

        fn dma_dealloc_pages(_addr: core::ptr::NonNull<u8>, _num_pages: usize) {}

        fn time_busy_wait(_dur: Duration) {}

        fn time_monotonic_nanos() -> u64 {
            0
        }

        fn time_try_init_epoch_offset(_epoch_time_nanos: u64) -> bool {
            false
        }

        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> KlibResult {
            Ok(())
        }

        fn irq_request_shared(_irq: IrqId, _handler: BoxedIrqHandler) -> KlibResult<IrqHandle> {
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
    }
}
