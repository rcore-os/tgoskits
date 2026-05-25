use log::info;
use rdrive::{PlatformDevice, probe::OnProbeError, register::FdtInfo};
use some_serial::{BSerial, ns16550, pl011};

crate::model_register!(
    name: "common serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["arm,pl011", "snps,dw-apb-uart"],
        on_probe: probe
    }],
);

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    info!("Probing serial device: {}", info.node.name());
    let base_reg =
        info.node.regs().into_iter().next().ok_or_else(|| {
            OnProbeError::other(alloc::format!("[{}] has no reg", info.node.name()))
        })?;

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    let mmio_base = crate::mmio::iomap(base_reg.address as usize, mmio_size as usize)?;

    let node = info.node.as_node();
    let clock_freq = prop_u32(node, "clock-frequency").unwrap_or(24_000_000);
    let reg_width = prop_u32(node, "reg-io-width").unwrap_or(1) as usize;
    let mut serial: Option<BSerial> = None;
    for compatible in node.compatibles() {
        if compatible == "arm,pl011" {
            serial = Some(pl011::Pl011::new_boxed(mmio_base, clock_freq));
            break;
        }

        if compatible == "snps,dw-apb-uart" {
            serial = Some(ns16550::Ns16550::new_mmio_boxed(
                mmio_base, clock_freq, reg_width,
            ));
            break;
        }
    }

    if let Some(serial) = serial {
        let base = serial.base_addr();
        info!("Serial@{base:#x} registered successfully");
        plat_dev.register(serial);
    }

    Ok(())
}

fn prop_u32(node: &fdt_edit::Node, name: &str) -> Option<u32> {
    node.get_property(name).and_then(|prop| prop.get_u32())
}
