use alloc::vec::Vec;

use log::info;
use rdrive::{PlatformDevice, probe::OnProbeError, register::FdtInfo};
use rockchip_npu::{Rknpu, RknpuConfig, RknpuType};
pub use rockchip_npu::{
    RknpuAction,
    ioctrl::{RknpuMemCreate, RknpuMemMap, RknpuMemSync, RknpuSubmit},
};
use rockchip_pm::{PowerDomain, RockchipPM};

use crate::mmio::iomap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Busy,
    InvalidData,
}

module_driver!(
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

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
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

    enable_pm();

    info!("NPU power enabled");

    let dma = axklib::dma::device(u32::MAX as u64);
    let npu = Rknpu::new(&base_regs, config, dma);
    plat_dev.register(npu);
    info!("NPU registered successfully");
    Ok(())
}

fn enable_pm() {
    let mut pm = rdrive::get_one::<RockchipPM>()
        .expect("RockchipPM not found")
        .lock()
        .expect("RockchipPM lock failed");

    // RK3588 NPU power domain IDs (from rockchip-pm rk3588 variant)
    pm.power_domain_on(PowerDomain(9)).unwrap(); // NPUTOP
    pm.power_domain_on(PowerDomain(8)).unwrap(); // NPU
    pm.power_domain_on(PowerDomain(10)).unwrap(); // NPU1
    pm.power_domain_on(PowerDomain(11)).unwrap(); // NPU2
}

pub fn is_available() -> bool {
    rdrive::get_one::<Rknpu>().is_some()
}

pub fn obj_addr_and_size(handle: u32) -> Result<(usize, usize), Error> {
    with_npu(|npu| npu.get_obj_addr_and_size(handle).ok_or(Error::NotFound))
}

pub fn submit(args: &mut RknpuSubmit) -> Result<(), Error> {
    with_npu(|npu| npu.submit_ioctrl(args).map_err(|_| Error::InvalidData))
}

pub fn mem_create(args: &mut RknpuMemCreate) -> Result<(), Error> {
    with_npu(|npu| npu.create(args).map_err(|_| Error::InvalidData))
}

pub fn mem_sync(args: &mut RknpuMemSync) -> Result<(), Error> {
    with_npu(|npu| npu.mem_sync(args).map_err(|_| Error::InvalidData))
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
