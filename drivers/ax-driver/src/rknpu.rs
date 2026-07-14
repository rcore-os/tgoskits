use alloc::{sync::Arc, vec::Vec};
use core::any::Any;

use log::info;
use rdrive::{probe::OnProbeError, register::ProbeFdt};
pub use rockchip_npu::{
    GemBufferInfo, GemCachePolicy, RknpuAction,
    ioctrl::{RknpuMemCreate, RknpuMemDestroy, RknpuMemMap, RknpuMemSync, RknpuSubmit},
};
use rockchip_npu::{Rknpu, RknpuConfig, RknpuType};

use crate::mmio::iomap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Busy,
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

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
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

    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let npu = Rknpu::new(&base_regs, config, dma);
    plat_dev.register(npu);
    info!("NPU registered successfully");
    Ok(())
}

pub fn is_available() -> bool {
    rdrive::get_one::<Rknpu>().is_some()
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
    with_npu(|npu| npu.submit_ioctrl(args).map_err(|_| Error::InvalidData))
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
/// an imported buffer. A missing handle is a no-op.
pub fn mem_destroy(handle: u32) -> Result<(), Error> {
    with_npu(|npu| {
        npu.destroy(handle);
        Ok(())
    })
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
    let mut npu = rdrive::get_one::<Rknpu>()
        .ok_or(Error::NotFound)?
        .try_lock()
        .map_err(|_| Error::Busy)?;
    f(&mut npu)
}
