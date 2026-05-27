use alloc::{format, string::String};

use fdt_edit::{Fdt, NodeType, RegFixed, Status};
use log::{info, warn};
use rdrive::{PlatformDevice, probe::OnProbeError, register::FdtInfo};
use some_serial::{
    BSerial, ns16550,
    ns16550::rockchip_fiq::{ROCKCHIP_FIQ_RK3588_UART_CLOCK, RockchipFiqConfig, RockchipFiqSerial},
    pl011,
};

crate::model_register!(
    name: "common serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["arm,pl011", "snps,dw-apb-uart"],
        on_probe: probe
    }],
);

crate::model_register!(
    name: "rockchip fiq debugger serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["rockchip,fiq-debugger"],
        on_probe: probe_rockchip_fiq
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

fn probe_rockchip_fiq(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let live_fdt =
        rdrive::with_fdt(Clone::clone).ok_or_else(|| OnProbeError::other("live FDT not found"))?;
    let fdt_config = rockchip_fiq_fdt_config(&live_fdt, info.node)?;
    let mmio_base = crate::mmio::iomap(
        fdt_config.reg.address as usize,
        fdt_config.reg.size.unwrap_or(0x100) as usize,
    )?;

    if fdt_config.target_disabled {
        info!(
            "Rockchip FIQ debugger takes disabled UART alias serial{} at {}",
            fdt_config.config.serial_id, fdt_config.uart_path
        );
    }

    let serial = RockchipFiqSerial::new_boxed(mmio_base, fdt_config.config);
    let base = serial.base_addr();
    info!(
        "Rockchip FIQ debugger UART@{base:#x} registered successfully, serial-id={}, baudrate={}, \
         irq-mode={}",
        fdt_config.config.serial_id, fdt_config.config.baudrate, fdt_config.config.irq_mode_enabled
    );
    plat_dev.register(serial);
    Ok(())
}

fn prop_u32(node: &fdt_edit::Node, name: &str) -> Option<u32> {
    node.get_property(name).and_then(|prop| prop.get_u32())
}

struct RockchipFiqFdtConfig {
    config: RockchipFiqConfig,
    reg: RegFixed,
    uart_path: String,
    target_disabled: bool,
}

fn rockchip_fiq_fdt_config(
    fdt: &Fdt,
    fiq: NodeType<'_>,
) -> Result<RockchipFiqFdtConfig, OnProbeError> {
    let fiq_node = fiq.as_node();
    let serial_id = prop_u32(fiq_node, "rockchip,serial-id").ok_or_else(|| {
        OnProbeError::other(format!("[{}] has no rockchip,serial-id", fiq.name()))
    })?;

    if serial_id == u32::MAX {
        return Err(OnProbeError::NotMatch);
    }

    let alias = format!("serial{serial_id}");
    let uart_path = fdt
        .resolve_alias(&alias)
        .map(String::from)
        .ok_or_else(|| OnProbeError::other(format!("{alias} alias not found")))?;
    let uart_node = fdt
        .get_by_path(&uart_path)
        .ok_or_else(|| OnProbeError::other(format!("{uart_path} node not found")))?;
    let uart = uart_node.as_node();

    if !uart
        .compatibles()
        .any(|compatible| compatible == "snps,dw-apb-uart")
    {
        return Err(OnProbeError::other(format!(
            "{uart_path} is not a snps,dw-apb-uart node"
        )));
    }

    let reg_width = prop_u32(uart, "reg-io-width").unwrap_or(4);
    let reg_shift = prop_u32(uart, "reg-shift").unwrap_or(2);
    if reg_width != 4 || reg_shift != 2 {
        return Err(OnProbeError::other(format!(
            "{uart_path} has unsupported reg-io-width/reg-shift {reg_width}/{reg_shift}"
        )));
    }

    let reg = uart_node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{uart_path}] has no reg")))?;

    let baudrate = normalise_fiq_baudrate(
        prop_u32(fiq_node, "rockchip,baudrate")
            .unwrap_or(some_serial::ns16550::rockchip_fiq::ROCKCHIP_FIQ_DEFAULT_BAUDRATE),
    );
    let clock_hz = prop_u32(uart, "clock-frequency").unwrap_or(ROCKCHIP_FIQ_RK3588_UART_CLOCK);
    let irq_mode_enabled = prop_u32(fiq_node, "rockchip,irq-mode-enable").unwrap_or(0) != 0;
    let target_disabled = matches!(uart.status(), Some(Status::Disabled));

    if matches!(uart.status(), Some(status) if status != Status::Disabled && status != Status::Okay)
    {
        warn!("{uart_path} has unrecognised status; proceeding for FIQ debugger");
    }

    Ok(RockchipFiqFdtConfig {
        config: RockchipFiqConfig {
            serial_id,
            baudrate,
            clock_hz,
            irq_mode_enabled,
            debug_enable: true,
            console_enable: true,
        },
        reg,
        uart_path,
        target_disabled,
    })
}

fn normalise_fiq_baudrate(baudrate: u32) -> u32 {
    match baudrate {
        115_200 | 1_500_000 => baudrate,
        other => {
            warn!("unsupported rockchip fiq baudrate {other}, falling back to 115200");
            115_200
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use fdt_edit::{Fdt, Node, Property};

    use super::*;

    #[test]
    fn resolves_fiq_debugger_target_uart_from_alias_even_when_uart_disabled() {
        let fdt = minimal_fiq_fdt(true, true);
        let fiq = fdt.get_by_path("/fiq-debugger").expect("fiq node missing");

        let config = rockchip_fiq_fdt_config(&fdt, fiq).expect("parse fiq config");

        assert_eq!(config.config.serial_id, 2);
        assert_eq!(config.config.baudrate, 1_500_000);
        assert_eq!(config.config.clock_hz, ROCKCHIP_FIQ_RK3588_UART_CLOCK);
        assert!(config.config.irq_mode_enabled);
        assert_eq!(config.uart_path, "/serial@feb50000");
        assert!(config.target_disabled);
        assert_eq!(config.reg.address, 0xfeb5_0000);
        assert_eq!(config.reg.size, Some(0x100));
    }

    #[test]
    fn rejects_missing_alias_or_non_dw_apb_target() {
        let fdt = minimal_fiq_fdt(false, true);
        let fiq = fdt.get_by_path("/fiq-debugger").expect("fiq node missing");
        assert!(rockchip_fiq_fdt_config(&fdt, fiq).is_err());

        let fdt = minimal_fiq_fdt(true, false);
        let fiq = fdt.get_by_path("/fiq-debugger").expect("fiq node missing");
        assert!(rockchip_fiq_fdt_config(&fdt, fiq).is_err());
    }

    fn minimal_fiq_fdt(with_alias: bool, dw_apb: bool) -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32_ls("#address-cells", &[2]));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32_ls("#size-cells", &[1]));

        let aliases = fdt.add_node(root, Node::new("aliases"));
        if with_alias {
            fdt.node_mut(aliases)
                .unwrap()
                .set_property(prop_str("serial2", "/serial@feb50000"));
        }

        let fiq = fdt.add_node(root, Node::new("fiq-debugger"));
        fdt.node_mut(fiq)
            .unwrap()
            .set_property(prop_strs("compatible", &["rockchip,fiq-debugger"]));
        fdt.node_mut(fiq)
            .unwrap()
            .set_property(prop_u32_ls("rockchip,serial-id", &[2]));
        fdt.node_mut(fiq)
            .unwrap()
            .set_property(prop_u32_ls("rockchip,baudrate", &[1_500_000]));
        fdt.node_mut(fiq)
            .unwrap()
            .set_property(prop_u32_ls("rockchip,irq-mode-enable", &[1]));
        fdt.node_mut(fiq)
            .unwrap()
            .set_property(prop_str("status", "okay"));

        let uart = fdt.add_node(root, Node::new("serial@feb50000"));
        fdt.node_mut(uart).unwrap().set_property(prop_strs(
            "compatible",
            if dw_apb {
                &["rockchip,rk3588-uart", "snps,dw-apb-uart"]
            } else {
                &["rockchip,rk3588-uart"]
            },
        ));
        fdt.node_mut(uart)
            .unwrap()
            .set_property(prop_reg(0xfeb5_0000, 0x100));
        fdt.node_mut(uart)
            .unwrap()
            .set_property(prop_u32_ls("reg-io-width", &[4]));
        fdt.node_mut(uart)
            .unwrap()
            .set_property(prop_u32_ls("reg-shift", &[2]));
        fdt.node_mut(uart)
            .unwrap()
            .set_property(prop_str("status", "disabled"));
        fdt
    }

    fn prop_u32_ls(name: &str, values: &[u32]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(&value.to_be_bytes());
        }
        Property::new(name, data)
    }

    fn prop_reg(address: u64, size: u32) -> Property {
        let mut data = Vec::new();
        data.extend_from_slice(&((address >> 32) as u32).to_be_bytes());
        data.extend_from_slice(&(address as u32).to_be_bytes());
        data.extend_from_slice(&size.to_be_bytes());
        Property::new("reg", data)
    }

    fn prop_str(name: &str, value: &str) -> Property {
        let mut data = Vec::new();
        data.extend_from_slice(value.as_bytes());
        data.push(0);
        Property::new(name, data)
    }

    fn prop_strs(name: &str, values: &[&str]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(value.as_bytes());
            data.push(0);
        }
        Property::new(name, data)
    }
}
