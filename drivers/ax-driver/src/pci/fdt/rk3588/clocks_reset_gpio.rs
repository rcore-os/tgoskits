use alloc::{format, vec::Vec};

use fdt_edit::{Node, Phandle};
use log::warn;
use rdrive::{
    probe::{
        OnProbeError,
        fdt::{ClockLine, NodeType, ResetLine, apply_assigned_clocks, clock_lines, reset_lines},
    },
    register::FdtInfo,
};

use super::{
    resources::{GpioSpec, RK3588_GPIO_BASES},
    windows::{live_fdt, rk3588_pcie_reset_pin},
};

pub(super) fn clock_lines_for_node(node: NodeType<'_>) -> Result<Vec<ClockLine>, OnProbeError> {
    apply_assigned_clocks(node)?;
    clock_lines(node)
}

pub(super) fn enable_clocks(clocks: &[ClockLine]) -> Result<(), OnProbeError> {
    for clock in clocks {
        let id = clock.id().raw();
        if id == 0 {
            continue;
        }
        clock.enable()?;
    }
    Ok(())
}

pub(super) fn parse_resets(node: NodeType<'_>) -> Result<Vec<ResetLine>, OnProbeError> {
    reset_lines(node)
}

pub(super) fn assert_resets(resets: &[ResetLine]) -> Result<(), OnProbeError> {
    for reset in resets {
        reset.assert()?;
    }
    Ok(())
}

pub(super) fn deassert_resets(resets: &[ResetLine]) -> Result<(), OnProbeError> {
    for reset in resets {
        reset.deassert()?;
    }
    Ok(())
}

pub(super) fn parse_reset_gpio(
    info: &FdtInfo<'_>,
    apb_base: u64,
) -> Result<Option<GpioSpec>, OnProbeError> {
    if let Some(gpio) = parse_gpio_spec(info.node, "reset-gpios")? {
        return Ok(Some(gpio));
    }

    if let Some(default) = rk3588_pcie_reset_pin(apb_base) {
        warn!(
            "Rockchip RK3588 PCIe host {:#x}: reset-gpios missing; using diagnostic fallback \
             GPIO{} pin {}",
            apb_base, default.bank, default.pin
        );
        return Ok(Some(GpioSpec {
            bank: default.bank,
            pin: default.pin,
            active_high: default.active_high,
        }));
    }

    Ok(None)
}

fn parse_gpio_spec(
    node_type: NodeType<'_>,
    prop_name: &str,
) -> Result<Option<GpioSpec>, OnProbeError> {
    let node = node_type.as_node();
    let Some(prop) = node.get_property(prop_name) else {
        return Ok(None);
    };
    let mut cells = prop.get_u32_iter();
    let phandle_raw = cells.next().ok_or_else(|| {
        OnProbeError::other(format!("[{}] has malformed {prop_name}", node.name()))
    })?;
    let pin = cells.next().ok_or_else(|| {
        OnProbeError::other(format!("[{}] has malformed {prop_name}", node.name()))
    })?;
    let flags = cells.next().unwrap_or(0);
    let bank = gpio_bank_from_phandle(Phandle::from(phandle_raw))?;
    Ok(Some(GpioSpec {
        bank,
        pin: pin.try_into().map_err(|_| {
            OnProbeError::other(format!(
                "[{}] {prop_name} pin {pin} does not fit RK3588 GPIO",
                node.name()
            ))
        })?,
        active_high: flags & 1 == 0,
    }))
}

fn gpio_bank_from_phandle(phandle: Phandle) -> Result<u8, OnProbeError> {
    let fdt = live_fdt()?;
    let gpio = fdt
        .get_by_phandle(phandle)
        .ok_or_else(|| OnProbeError::other(format!("GPIO phandle {phandle:?} not found")))?;
    gpio_bank_index(gpio.as_node()).ok_or_else(|| {
        OnProbeError::other(format!(
            "failed to resolve RK3588 GPIO bank for phandle {phandle:?}"
        ))
    })
}

fn gpio_bank_index(node: &Node) -> Option<u8> {
    let name = node.name();
    if let Some(name) = name
        .strip_prefix("gpio")
        .filter(|name| !name.starts_with('@'))
        && let Some(bank) = name
            .chars()
            .next()
            .and_then(|ch| ch.to_digit(10))
            .and_then(|bank| u8::try_from(bank).ok())
            .filter(|bank| usize::from(*bank) < RK3588_GPIO_BASES.len())
    {
        return Some(bank);
    }

    let address = gpio_bank_address(node)?;
    RK3588_GPIO_BASES
        .iter()
        .position(|base| *base == address)
        .and_then(|bank| u8::try_from(bank).ok())
}

fn gpio_bank_address(node: &Node) -> Option<u64> {
    if let Some(address) = node
        .name()
        .split_once('@')
        .and_then(|(_, unit)| u64::from_str_radix(unit, 16).ok())
    {
        return Some(address);
    }

    let reg = node.get_property("reg")?.get_u32_iter().collect::<Vec<_>>();
    match reg.as_slice() {
        [addr] => Some(u64::from(*addr)),
        cells if cells.len() >= 2 => Some((u64::from(cells[0]) << 32) | u64::from(cells[1])),
        _ => None,
    }
}
