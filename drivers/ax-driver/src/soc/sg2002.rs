use log::info;
use rdrive::{PlatformDevice, probe::OnProbeError, register::FdtInfo};

const PLACEHOLDER_COMPATIBLES: &[&str] = &[
    "cvitek,tpu",
    "cvitek,cvitek-ion",
    "cvitek,cvi-pwm",
    "snps,dw-apb-gpio",
    "cvitek,cv182x-usb",
    "cvitek,cif",
    "cvitek,mipi_tx",
    "cvitek,sys",
    "cvitek,base",
    "cvitek,vi",
    "cvitek,vpss",
    "cvitek,ive",
    "cvitek,vo",
    "cvitek,fb",
    "cvitek,dwa",
    "cvitek,asic-vcodec",
    "cvitek,asic-jpeg",
    "cvitek,cvi_vc_drv",
    "cvitek,rtos_cmdqu",
    "cvitek,audio",
    "cvitek,cv181x-thermal",
];

crate::model_register!(
    name: "SG2002 placeholder",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["cvitek,cv181x"],
        on_probe: probe,
    }],
);

fn probe(info: FdtInfo<'_>, _plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let mut mapped = 0usize;
    for node in info.find_compatible(PLACEHOLDER_COMPATIBLES) {
        for reg in node.regs() {
            let Some(size) = reg.size else {
                continue;
            };
            if size == 0 {
                continue;
            }
            let mmio = crate::mmio::iomap(reg.address as usize, size as usize)?;
            mapped += 1;
            info!(
                "SG2002 placeholder mapped {}: {:#x}+{:#x} -> {:#x}",
                node.name(),
                reg.address,
                size,
                mmio.as_ptr() as usize
            );
        }
    }

    info!("SG2002 placeholder mapped {mapped} region(s)");
    Ok(())
}
