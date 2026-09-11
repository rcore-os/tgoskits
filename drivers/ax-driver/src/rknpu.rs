use alloc::{format, sync::Arc, vec::Vec};
use core::any::Any;

use log::info;
use rdrive::{
    DriverGeneric,
    probe::{
        OnProbeError,
        fdt::{FdtInfo, ResetLine},
    },
    register::ProbeFdt,
};
pub use rockchip_npu::{
    GemBufferInfo, GemCachePolicy, RknpuAction,
    ioctrl::{RknpuMemCreate, RknpuMemDestroy, RknpuMemMap, RknpuMemSync, RknpuSubmit},
};
use rockchip_npu::{Rknpu, RknpuConfig, RknpuType};

use crate::mmio::iomap;

const RK3588_NPU_CLOCK_NAME: &str = "clk_npu";
// Highest RK3588 NPU OPP that needs no more than the firmware-provided 750 mV.
const RK3588_NPU_FIXED_RATE_HZ: u64 = 800_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("Rockchip NPU is unavailable or the requested object was not found")]
    NotFound,
    #[error("Rockchip NPU is busy")]
    Busy,
    #[error("Rockchip NPU request timed out")]
    TimedOut,
    #[error("Rockchip NPU reset failed after a timed-out submission; the device is quarantined")]
    Quarantined,
    #[error("Rockchip NPU request contains invalid data")]
    InvalidData,
}

crate::model_register!(
    name: "Rockchip NPU",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-rknpu"],
            on_probe: probe
        }
    ],
);

struct RknpuDevice {
    core: Rknpu,
    resets: Vec<ResetLine>,
    state: DeviceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceState {
    Operational,
    Quarantined,
}

impl DeviceState {
    fn ensure_operational(self) -> Result<(), Error> {
        if matches!(self, Self::Quarantined) {
            Err(Error::Quarantined)
        } else {
            Ok(())
        }
    }
}

impl RknpuDevice {
    fn ensure_available(&self) -> Result<(), Error> {
        self.state.ensure_operational()
    }

    fn recover_timeout(&mut self) -> Result<(), Error> {
        recover_after_timeout(&mut self.state, || {
            for reset in &self.resets {
                reset.reset().map_err(|err| {
                    log::error!(
                        "RKNPU reset {:?} ({:#x}) failed after timeout: {err}",
                        reset.name(),
                        reset.id().raw()
                    );
                    Error::Quarantined
                })?;
            }
            Ok(())
        })
    }

    fn destroy_gem(&mut self, handle: u32) -> Result<(), Error> {
        cleanup_gem_if_safe(self.state, || self.core.destroy(handle))
    }
}

fn recover_after_timeout(
    state: &mut DeviceState,
    reset_all_cores: impl FnOnce() -> Result<(), Error>,
) -> Result<(), Error> {
    if let Err(err) = reset_all_cores() {
        quarantine_device(state);
        return Err(err);
    }
    Ok(())
}

fn quarantine_device(state: &mut DeviceState) {
    *state = DeviceState::Quarantined;
}

fn cleanup_gem_if_safe<T>(state: DeviceState, cleanup: impl FnOnce() -> T) -> Result<T, Error> {
    state.ensure_operational()?;
    Ok(cleanup())
}

impl DriverGeneric for RknpuDevice {
    fn name(&self) -> &str {
        self.core.name()
    }
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    configure_fixed_clock(&info)?;
    let regs = info.node.regs();

    let config = RknpuConfig {
        rknpu_type: RknpuType::Rk3588,
    };

    let mut base_regs = Vec::new();
    let page_size = 0x1000;
    for reg in &regs {
        let start_raw = reg.address as usize;
        let end = start_raw + reg.size.unwrap_or(0x1000) as usize;

        let start = start_raw & !(page_size - 1);
        let offset = start_raw - start;
        let end = (end + page_size - 1) & !(page_size - 1);
        let size = end - start;

        base_regs.push(unsafe { iomap(start, size)?.add(offset) });
    }

    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        crate::binding_resolver::dma_coherency_from_fdt(&info),
        dma_api::DmaConstraints::new(u32::MAX as u64),
    ));
    let resets = info.reset_lines()?;
    if resets.is_empty() {
        return Err(OnProbeError::other("RKNPU node has no reset line"));
    }
    let npu = RknpuDevice {
        core: Rknpu::new(&base_regs, config, dma),
        resets,
        state: DeviceState::Operational,
    };
    plat_dev.register(npu);
    info!("NPU registered successfully");
    Ok(())
}

fn configure_fixed_clock(info: &FdtInfo<'_>) -> Result<(), OnProbeError> {
    let clock = info
        .find_clock_line_by_name(RK3588_NPU_CLOCK_NAME)?
        .ok_or_else(|| OnProbeError::other("RK3588 NPU clk_npu is unavailable"))?;
    clock.set_rate(RK3588_NPU_FIXED_RATE_HZ)?;
    let actual_rate = clock.rate()?;
    if actual_rate != RK3588_NPU_FIXED_RATE_HZ {
        return Err(OnProbeError::other(format!(
            "RK3588 NPU clock rate mismatch: requested {} Hz, got {actual_rate} Hz",
            RK3588_NPU_FIXED_RATE_HZ
        )));
    }
    info!("RK3588 NPU fixed clock rate: {actual_rate} Hz");
    Ok(())
}

pub fn is_available() -> bool {
    rdrive::get_one::<RknpuDevice>().is_some()
}

pub fn obj_addr_and_size(handle: u32) -> Result<(usize, usize), Error> {
    with_npu(|npu| npu.get_obj_addr_and_size(handle).ok_or(Error::NotFound))
}

pub fn buffer_info(handle: u32) -> Result<GemBufferInfo, Error> {
    with_npu(|npu| npu.get_buffer_info(handle).ok_or(Error::NotFound))
}

/// A lifetime retainer for the buffer backing `handle`. Holding the returned
/// `Arc` keeps the backing allocation alive independent of the GEM pool, so a
/// mapping can outlive a `MemDestroy` without dangling.
pub fn buffer_retainer(handle: u32) -> Result<Arc<dyn Any + Send + Sync>, Error> {
    with_npu(|npu| npu.buffer_retainer(handle).ok_or(Error::NotFound))
}

pub fn submit(args: &mut RknpuSubmit) -> Result<(), Error> {
    let mut npu = rdrive::get_one::<RknpuDevice>()
        .ok_or(Error::NotFound)?
        .try_lock()
        .map_err(|_| Error::Busy)?;
    npu.ensure_available()?;
    let mut clock = axklib::time::monotonic_nanos;
    match npu.core.submit_ioctrl(args, &mut clock) {
        Ok(()) => Ok(()),
        Err(rockchip_npu::RknpuError::Timeout) => {
            npu.recover_timeout()?;
            Err(Error::TimedOut)
        }
        Err(_) => Err(Error::InvalidData),
    }
}

pub fn mem_create(args: &mut RknpuMemCreate) -> Result<(), Error> {
    with_npu(|npu| npu.create(args).map_err(|_| Error::InvalidData))
}

/// Import an externally-owned, physically-contiguous buffer (resolved from a
/// dma-buf fd) into the GEM pool, returning a handle the rest of the NPU ABI
/// (`MemMap`/`mmap`/submit) resolves like any other. `retainer` keeps the
/// exporter's allocation alive for the handle's lifetime.
pub fn mem_import(
    dma_addr: u64,
    obj_addr: usize,
    size: usize,
    flags: u32,
    retainer: Arc<dyn Any + Send + Sync>,
) -> Result<u32, Error> {
    with_npu(|npu| Ok(npu.import(dma_addr, obj_addr, size, flags, retainer)))
}

pub fn mem_sync(args: &mut RknpuMemSync) -> Result<(), Error> {
    with_npu(|npu| npu.mem_sync(args).map_err(|_| Error::InvalidData))
}

/// Release a GEM handle, freeing an owned allocation or dropping the retainer of
/// an imported buffer. A missing handle is a no-op. If the device is quarantined,
/// this returns [`Error::Quarantined`] and keeps the handle because the device may
/// still be accessing its backing allocation.
pub fn mem_destroy(handle: u32) -> Result<(), Error> {
    let mut npu = rdrive::get_one::<RknpuDevice>()
        .ok_or(Error::NotFound)?
        .try_lock()
        .map_err(|_| Error::Busy)?;
    npu.destroy_gem(handle)
}

pub fn mem_map_offset(handle: u32) -> Result<u64, Error> {
    with_npu(|npu| {
        npu.get_phys_addr_and_size(handle)
            .map(|_| (handle as u64) << 12)
            .ok_or(Error::InvalidData)
    })
}

pub fn action(flags: RknpuAction) -> Result<u32, Error> {
    with_npu(|npu| npu.action(flags).map_err(|_| Error::InvalidData))
}

fn with_npu<F, R>(f: F) -> Result<R, Error>
where
    F: FnOnce(&mut Rknpu) -> Result<R, Error>,
{
    let mut npu = rdrive::get_one::<RknpuDevice>()
        .ok_or(Error::NotFound)?
        .try_lock()
        .map_err(|_| Error::Busy)?;
    npu.ensure_available()?;
    f(&mut npu.core)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_timeout_recovery_quarantines_the_device() {
        let mut state = DeviceState::Operational;
        let mut reset_attempted = false;

        assert_eq!(
            recover_after_timeout(&mut state, || {
                reset_attempted = true;
                Err(Error::Quarantined)
            }),
            Err(Error::Quarantined)
        );

        assert!(reset_attempted);
        assert_eq!(state, DeviceState::Quarantined);
        assert_eq!(state.ensure_operational(), Err(Error::Quarantined));
    }

    #[test]
    fn failed_reset_keeps_gem_backing_for_deferred_cleanup() {
        use core::sync::atomic::{AtomicBool, Ordering};

        struct DropSpy(Arc<AtomicBool>);

        impl Drop for DropSpy {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let mut state = DeviceState::Operational;
        assert_eq!(
            recover_after_timeout(&mut state, || Err(Error::Quarantined)),
            Err(Error::Quarantined)
        );

        let released = Arc::new(AtomicBool::new(false));
        let mut backing = Some(DropSpy(released.clone()));
        let result = cleanup_gem_if_safe(state, || backing.take());

        assert!(matches!(result, Err(Error::Quarantined)));
        assert!(backing.is_some());
        assert!(!released.load(Ordering::SeqCst));

        drop(backing);
        assert!(released.load(Ordering::SeqCst));
    }
}
