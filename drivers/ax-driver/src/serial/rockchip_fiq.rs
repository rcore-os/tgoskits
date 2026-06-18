use alloc::{format, string::String};

use fdt_edit::{Fdt, NodeType, RegFixed, Status};
use log::{info, warn};
use rdrive::{probe::OnProbeError, register::ProbeFdt};
use some_serial::ns16550::rockchip_fiq::{
    ROCKCHIP_FIQ_DEFAULT_BAUDRATE, ROCKCHIP_FIQ_RK3588_UART_CLOCK, RockchipFiqConfig,
    RockchipFiqSerial,
};

use super::{PlatformSerialDevice, SerialDeviceInfo, prop_u32};
use crate::BindingInfo;

model_register!(
    name: "rockchip fiq debugger serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["rockchip,fiq-debugger"],
        on_probe: probe
    }],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
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
    let binding_info = if fdt_config.config.irq_mode_enabled {
        uart_binding_info(&live_fdt, &fdt_config.uart_path)?
    } else {
        BindingInfo::empty()
    };
    let device_id = plat_dev.descriptor().device_id();
    if !rdrive::note_fdt_device_path(&fdt_config.uart_path, device_id) {
        warn!(
            "failed to map Rockchip FIQ target UART path {} to serial device id {:?}",
            fdt_config.uart_path, device_id
        );
    }
    plat_dev.register(PlatformSerialDevice::new(
        serial.name().into(),
        SerialDeviceInfo {
            fdt_path: fdt_config.uart_path,
            alias_index: Some(fdt_config.config.serial_id as usize),
            paddr: fdt_config.reg.address as usize,
            mapped_base: base,
            baudrate: serial.baudrate(),
            irq_num: binding_info.irq_num(),
            rx_polling_required: false,
            binding_info,
        },
        serial,
    ));
    Ok(())
}

fn uart_binding_info(fdt: &Fdt, uart_path: &str) -> Result<BindingInfo, OnProbeError> {
    let uart = fdt
        .get_by_path(uart_path)
        .ok_or_else(|| OnProbeError::other(format!("{uart_path} node not found")))?;
    let Some(interrupt) = uart.interrupts().into_iter().next() else {
        warn!("{uart_path} has no UART IRQ; FIQ serial tty will not be interrupt driven");
        return Ok(BindingInfo::empty());
    };
    let interrupt_parent = rdrive::fdt_phandle_to_device_id(interrupt.interrupt_parent)
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "failed to resolve interrupt parent {:?} for {uart_path}",
                interrupt.interrupt_parent
            ))
        })?;
    let intc = rdrive::get::<rdif_intc::Intc>(interrupt_parent).map_err(|err| {
        OnProbeError::other(format!(
            "failed to get interrupt controller {:?} for {uart_path}: {err:?}",
            interrupt_parent
        ))
    })?;
    let mut intc = intc.lock().map_err(|err| {
        OnProbeError::other(format!(
            "failed to lock interrupt controller {:?} for {uart_path}: {err:?}",
            interrupt_parent
        ))
    })?;
    Ok(BindingInfo::with_irq(Some(
        intc.setup_irq_by_fdt(&interrupt.specifier).into(),
    )))
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
        prop_u32(fiq_node, "rockchip,baudrate").unwrap_or(ROCKCHIP_FIQ_DEFAULT_BAUDRATE),
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
