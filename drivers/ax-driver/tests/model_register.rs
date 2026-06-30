#![feature(used_with_arg)]

use ax_driver::{PlatformDevice, probe::OnProbeError};
#[cfg(feature = "plat-dyn")]
use axklib::{
    AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle, IrqId,
    Klib, PhysAddr, VirtAddr, impl_trait,
};

#[cfg(feature = "plat-dyn")]
struct KlibImpl;

#[cfg(feature = "plat-dyn")]
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

        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> axklib::AxResult {
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

ax_driver::model_register!(
    name: "ax-driver model register test",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe,
    }],
);

fn probe(_plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    Ok(())
}

#[test]
fn model_register_is_usable_from_ax_driver_only() {
    let _ = core::mem::size_of::<ax_driver::register::DriverRegister>();
}
