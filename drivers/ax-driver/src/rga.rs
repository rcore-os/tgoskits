use alloc::vec::Vec;

use log::{info, warn};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use rockchip_pm::{PowerDomain, RockchipPM};
use rockchip_rga::{RgaCoreConfig, RgaCoreResource, RgaVersion, RockchipRga};

use crate::mmio::iomap;

crate::model_register!(
    name: "Rockchip RGA",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "rockchip,rga3_core0",
                "rockchip,rga3_core1",
                "rockchip,rga2_core0",
            ],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let config = detect_core_config(&info)
        .ok_or_else(|| OnProbeError::other("unsupported Rockchip RGA compatible string"))?;

    // Power on the core's domain BEFORE any RGA MMIO (RgaCore::new reads the version register).
    enable_power(config);

    let irq = crate::binding_info_from_fdt(&info)?.irq_num();

    let mut resources = Vec::new();
    for reg in info.node.regs() {
        let start_raw = reg.address as usize;
        let size_raw = reg.size.unwrap_or(0x1000) as usize;
        let (start, size, offset) = page_aligned_region(start_raw, size_raw);
        let base = unsafe { iomap(start, size)?.add(offset) };
        resources.push(RgaCoreResource {
            base,
            size: size_raw,
            irq,
            config,
        });
    }

    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let rga = RockchipRga::new(&resources, dma);
    let core_count = rga.core_count();
    let version = rga.cores().first().map(|c| c.version());
    plat_dev.register(rga);
    info!("RGA registered: cores={core_count} version={version:?}");
    Ok(())
}

fn enable_power(config: RgaCoreConfig) {
    // RK3588 power-domain IDs (from rockchip-pm rk3588 variant): RGA30=22, RGA31=30, RGA2/VDPU=21.
    let domain = match config.version {
        RgaVersion::Rga3 => {
            if config.core_index == 0 {
                PowerDomain(22) // RGA30
            } else {
                PowerDomain(30) // RGA31
            }
        }
        RgaVersion::Rga2 => PowerDomain(21), // RGA2/VDPU
    };
    match rdrive::get_one::<RockchipPM>() {
        Some(pm) => match pm.lock() {
            Ok(mut pm) => {
                if let Err(e) = pm.power_domain_on(domain) {
                    warn!("RGA power_domain_on({domain:?}) failed: {e:?}");
                }
            }
            Err(e) => warn!("RGA: RockchipPM lock failed: {e:?}"),
        },
        None => warn!("RGA: RockchipPM not found; assuming domain already powered"),
    }
    // Clocks/resets are intentionally NOT enabled here yet — confirm on board whether PM-only suffices
    // (the NPU probe gets away with PM-only). Add rockchip-soc Cru/Reset calls only if the board needs them.
}

fn detect_core_config(info: &FdtInfo<'_>) -> Option<RgaCoreConfig> {
    for compatible in info.node.as_node().compatibles() {
        match compatible {
            "rockchip,rga3_core0" => return Some(RgaCoreConfig::rga3(0)),
            "rockchip,rga3_core1" => return Some(RgaCoreConfig::rga3(1)),
            "rockchip,rga2_core0" => return Some(RgaCoreConfig::rga2(0)),
            _ => {}
        }
    }
    None
}

fn page_aligned_region(start_raw: usize, size_raw: usize) -> (usize, usize, usize) {
    let page_size = 0x1000;
    let start = start_raw & !(page_size - 1);
    let offset = start_raw - start;
    let end = (start_raw + size_raw + page_size - 1) & !(page_size - 1);
    (start, end - start, offset)
}
