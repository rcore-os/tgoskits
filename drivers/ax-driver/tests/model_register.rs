#![feature(used_with_arg)]

use ax_driver::{
    probe::OnProbeError,
    register::{ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
};
use ax_kspin_test_runtime as _;
use axklib::{
    AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle, IrqId,
    Klib, PhysAddr, VirtAddr, impl_trait,
};

struct KlibImpl;

impl_trait! {
    impl Klib for KlibImpl {
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

        // The host integration-test DMA model is cache-coherent. Define the
        // symbols required by trait-FFI without simulating non-coherent cache.
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

ax_driver::model_register!(
    name: "ax-driver model register test",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,model-register"],
        on_probe: probe,
    }],
);

fn probe(_probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    Ok(())
}

#[test]
fn model_register_is_usable_from_ax_driver_only() {
    let _ = core::mem::size_of::<ax_driver::register::DriverRegister>();
}
