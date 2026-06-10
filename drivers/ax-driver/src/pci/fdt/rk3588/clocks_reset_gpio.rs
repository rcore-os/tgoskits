use alloc::{format, vec::Vec};

use fdt_edit::{ClockRef, Node, Phandle};
use log::warn;
use rdrive::{
    probe::{OnProbeError, fdt::NodeType},
    register::FdtInfo,
};

use super::{
    resources::{ClockSpec, GpioSpec, RK3588_GPIO_BASES, ResetSpec},
    windows::{live_fdt, prop_str_list, rk3588_pcie_reset_pin},
};
use crate::soc::{
    rk3588_enable_clock, rk3588_reset_assert, rk3588_reset_deassert, rk3588_set_clock_rate,
};

pub(super) fn clock_specs_for_node(node: NodeType<'_>) -> Vec<ClockSpec> {
    let assigned_clocks = node
        .as_node()
        .get_property("assigned-clocks")
        .map(|prop| {
            let vals = prop.get_u32_iter().collect::<Vec<_>>();
            let mut ids = Vec::new();
            for cells in vals.chunks(2) {
                if let [_, id] = cells {
                    ids.push(*id);
                }
            }
            ids
        })
        .unwrap_or_default();
    let assigned_rates = node
        .as_node()
        .get_property("assigned-clock-rates")
        .map(|prop| prop.get_u32_iter().collect::<Vec<_>>())
        .unwrap_or_default();

    node.clocks()
        .into_iter()
        .filter_map(|clock| {
            let assigned_rate = clock.specifier.first().and_then(|id| {
                assigned_clocks
                    .iter()
                    .position(|assigned| assigned == id)
                    .and_then(|index| assigned_rates.get(index).copied())
                    .filter(|rate| *rate != 0)
            });
            let id = *clock.specifier.first()?;
            Some(ClockSpec {
                name: clock.name,
                id,
                assigned_rate,
            })
        })
        .collect()
}

pub(super) fn clock_specs(clocks: Vec<ClockRef>) -> Vec<ClockSpec> {
    clocks
        .into_iter()
        .filter_map(|clock| {
            let id = *clock.specifier.first()?;
            Some(ClockSpec {
                name: clock.name,
                id,
                assigned_rate: None,
            })
        })
        .collect()
}

pub(super) fn enable_clocks(clocks: &[ClockSpec]) -> Result<(), OnProbeError> {
    for clock in clocks {
        let id = clock.id;
        if id == 0 {
            continue;
        }
        if let Some(rate) = clock.assigned_rate {
            rk3588_set_clock_rate(id, u64::from(rate)).map_err(|err| {
                OnProbeError::other(format!(
                    "failed to set RK3588 PCIe clock {:?} ({id:#x}) rate to {rate}: {err}",
                    clock.name
                ))
            })?;
        }
        rk3588_enable_clock(id).map_err(|err| {
            OnProbeError::other(format!(
                "failed to enable RK3588 PCIe clock {:?} ({id:#x}): {err}",
                clock.name
            ))
        })?;
    }
    Ok(())
}

pub(super) fn parse_resets(node: NodeType<'_>) -> Result<Vec<ResetSpec>, OnProbeError> {
    let Some(prop) = node.as_node().get_property("resets") else {
        return Ok(Vec::new());
    };
    let cells = prop.get_u32_iter().collect::<Vec<_>>();
    if cells.len() % 2 != 0 {
        return Err(OnProbeError::other(format!(
            "[{}] has malformed resets",
            node.name()
        )));
    }
    let reset_names = prop_str_list(node.as_node(), "reset-names");
    Ok(cells
        .chunks(2)
        .enumerate()
        .map(|(idx, chunk)| ResetSpec {
            name: reset_names.get(idx).cloned(),
            id: u64::from(chunk[1]),
        })
        .collect())
}

pub(super) fn assert_resets(resets: &[ResetSpec]) -> Result<(), OnProbeError> {
    for reset in resets {
        rk3588_reset_assert(reset.id).map_err(|err| {
            OnProbeError::other(format!(
                "failed to assert RK3588 PCIe reset {:?} ({:#x}): {err}",
                reset.name, reset.id
            ))
        })?;
    }
    Ok(())
}

pub(super) fn deassert_resets(resets: &[ResetSpec]) -> Result<(), OnProbeError> {
    for reset in resets {
        rk3588_reset_deassert(reset.id).map_err(|err| {
            OnProbeError::other(format!(
                "failed to deassert RK3588 PCIe reset {:?} ({:#x}): {err}",
                reset.name, reset.id
            ))
        })?;
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
