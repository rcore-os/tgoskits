use alloc::vec::Vec;

use log::info;
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use rockchip_rga::{RgaCoreConfig, RgaCoreResource, RockchipRga};

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

    let mut resources = Vec::new();
    for reg in info.node.regs() {
        let start_raw = reg.address as usize;
        let size_raw = reg.size.unwrap_or(0x1000) as usize;
        let (start, size, offset) = page_aligned_region(start_raw, size_raw);
        let base = unsafe { iomap(start, size)?.add(offset) };

        resources.push(RgaCoreResource {
            base,
            size: size_raw,
            irq: decode_fdt_irq(&info.interrupts()),
            config,
        });
    }

    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let rga = RockchipRga::new(&resources, dma);
    let core_count = rga.core_count();
    plat_dev.register(rga);

    info!("RGA registered successfully, cores={core_count}");
    Ok(())
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

fn decode_fdt_irq(interrupts: &[rdrive::probe::fdt::InterruptRef]) -> Option<usize> {
    let interrupt = interrupts.first()?;
    match interrupt.specifier.as_slice() {
        [irq] => Some(*irq as usize),
        [kind, irq, ..] => match *kind {
            0 => Some(*irq as usize + 32),
            1 => Some(*irq as usize + 16),
            _ => Some(*irq as usize),
        },
        _ => None,
    }
}
