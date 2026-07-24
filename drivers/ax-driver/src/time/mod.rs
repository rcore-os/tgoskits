use log::{debug, info};
use rdrive::probe::OnProbeError;

#[cfg(target_arch = "x86_64")]
mod cmos;
#[cfg(axtest)]
pub(crate) use self::cmos::cmos_register_constants_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::cmos::cmos_io_struct_and_constants_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::cmos::cmos_register_edge_cases_hold_for_test;
#[cfg(any(test, target_arch = "x86_64"))]
mod cmos_decode;
#[cfg(any(
    test,
    target_arch = "loongarch64",
    target_arch = "riscv64",
    target_arch = "x86_64"
))]
mod datetime;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
mod fdt;
#[cfg(target_arch = "riscv64")]
mod goldfish;
#[cfg(target_arch = "loongarch64")]
mod loongson;
#[cfg(any(test, target_arch = "loongarch64"))]
mod loongson_decode;
#[cfg(target_arch = "aarch64")]
mod pl031;
#[cfg(target_arch = "riscv64")]
mod starfive;
#[cfg(any(test, target_arch = "riscv64"))]
mod starfive_decode;

fn init_epoch_offset(node_name: &str, unix_timestamp: u64) -> Result<(), OnProbeError> {
    if unix_timestamp == 0 {
        return Err(OnProbeError::other(alloc::format!(
            "[{node_name}] returned zero unix timestamp"
        )));
    }

    let epoch_time_nanos = unix_timestamp * 1_000_000_000;
    if axklib::time::try_init_epoch_offset(epoch_time_nanos) {
        info!("Initialized wall clock from {node_name}");
    } else {
        debug!("Skipping RTC {node_name} because epoch offset is already initialized",);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use axklib::{
        AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle,
        IrqId, Klib, PhysAddr, VirtAddr, impl_trait,
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
}
