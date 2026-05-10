use alloc::vec::Vec;

use dma_api::DeviceDma;
use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};
use rockchip_npu::{Rknpu, RknpuConfig, RknpuType};
use rockchip_pm::{PowerDomain, RockchipPM};

use crate::drivers::{DmaImpl, iomap};

static DMA: DmaImpl = DmaImpl;

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

        base_regs.push(unsafe { iomap(start.into(), size)?.add(offset) });
    }

    enable_pm();

    info!("NPU power enabled");

    let dma = DeviceDma::new(u32::MAX as u64, &DMA);
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
