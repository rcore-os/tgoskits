extern crate alloc;

use alloc::{format, string::ToString, vec, vec::Vec};
use core::ptr::NonNull;

use fdt_edit::{Fdt, Node, NodeType, Phandle, RegFixed};
use log::{info, warn};
use rdrive::{DriverGeneric, probe::OnProbeError, register::ProbeFdt};
use rockchip_soc::{
    BankId, GpioDirection, Iomux, PinConfig, PinCtrl, PinCtrlOp, PinId, Pull, SocType,
};

use crate::mmio::iomap;

const DRIVER_NAME: &str = "rk3588-pinctrl";
const GPIO_BANK_COUNT: usize = 5;

crate::model_register!(
    name: "Rockchip PinCtrl",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-pinctrl"],
            on_probe: probe
        }
    ],
);

pub struct RockchipPinCtrl {
    inner: PinCtrl,
}

unsafe impl Send for RockchipPinCtrl {}

impl RockchipPinCtrl {
    fn new(inner: PinCtrl) -> Self {
        Self { inner }
    }

    pub fn apply_default_pinctrl(
        &mut self,
        node: NodeType<'_>,
    ) -> Result<Vec<PinId>, OnProbeError> {
        let node_name = node.name().to_string();
        let Some(prop) = node.as_node().get_property("pinctrl-0") else {
            info!("Rockchip node {node_name} has no default pinctrl");
            return Ok(Vec::new());
        };

        let mut configured = Vec::new();
        for phandle in prop.get_u32_iter().map(Phandle::from) {
            configured.extend(self.apply_pinctrl_phandle(phandle)?);
        }
        info!(
            "Rockchip node {node_name} applied {} default pinctrl pins",
            configured.len()
        );
        Ok(configured)
    }

    pub fn enable_fixed_regulator(&mut self, phandle: Phandle) -> Result<(), OnProbeError> {
        let fdt = live_fdt()?;
        let node = fdt.get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!("regulator phandle {phandle:?} not found"))
        })?;
        let node_name = node.name().to_string();
        let active_value = fixed_regulator_active_value(node.as_node());
        let gpio_active_low = gpio_active_low(node.as_node());
        let drive_value = if gpio_active_low {
            !active_value
        } else {
            active_value
        };
        let pins = if let Some(gpio) = parse_gpio_pin(&fdt, node.as_node(), "gpios")
            .or_else(|| parse_gpio_pin(&fdt, node.as_node(), "gpio"))
        {
            vec![gpio?]
        } else {
            let pinctrls = node
                .as_node()
                .get_property("pinctrl-0")
                .ok_or_else(|| OnProbeError::other(format!("[{node_name}] has no enable GPIO")))?
                .get_u32_iter()
                .map(Phandle::from)
                .collect::<Vec<_>>();

            let mut pins = Vec::new();
            for pinctrl in pinctrls {
                pins.extend(self.apply_pinctrl_phandle(pinctrl)?);
            }
            if pins.is_empty() {
                return Err(OnProbeError::other(format!(
                    "[{node_name}] pinctrl-0 did not configure any GPIO"
                )));
            }
            pins
        };

        for pin in pins {
            self.inner
                .set_gpio_direction(pin, GpioDirection::Output(drive_value))
                .map_err(|err| {
                    OnProbeError::other(format!(
                        "failed to set [{node_name}] GPIO {pin:?} direction: {err}"
                    ))
                })?;
            self.inner.write_gpio(pin, drive_value).map_err(|err| {
                OnProbeError::other(format!("failed to drive [{node_name}] GPIO {pin:?}: {err}"))
            })?;
        }

        let startup_delay_us = node
            .as_node()
            .get_property("startup-delay-us")
            .and_then(|prop| prop.get_u32())
            .unwrap_or(0);
        if startup_delay_us != 0 {
            axklib::time::busy_wait(core::time::Duration::from_micros(u64::from(
                startup_delay_us,
            )));
        }

        info!("Rockchip fixed regulator {node_name} enabled via pinctrl");
        Ok(())
    }

    pub fn apply_pinctrl_path(&mut self, path: &str) -> Result<Vec<PinId>, OnProbeError> {
        let fdt = live_fdt()?;
        let node = fdt
            .get_by_path(path)
            .ok_or_else(|| OnProbeError::other(format!("pinctrl path {path} not found")))?;
        self.apply_pinctrl_node(node)
    }

    fn apply_pinctrl_phandle(&mut self, phandle: Phandle) -> Result<Vec<PinId>, OnProbeError> {
        let fdt = live_fdt()?;
        let node = fdt
            .get_by_phandle(phandle)
            .ok_or_else(|| OnProbeError::other(format!("pinctrl phandle {phandle:?} not found")))?;
        self.apply_pinctrl_node(node)
    }

    fn apply_pinctrl_node(&mut self, node: NodeType<'_>) -> Result<Vec<PinId>, OnProbeError> {
        let pins = node
            .as_node()
            .get_property("rockchip,pins")
            .ok_or_else(|| OnProbeError::other(format!("[{}] has no rockchip,pins", node.name())))?
            .get_u32_iter()
            .collect::<Vec<_>>();
        if pins.len() % 4 != 0 {
            return Err(OnProbeError::other(format!(
                "[{}] has malformed rockchip,pins with {} cells",
                node.name(),
                pins.len()
            )));
        }

        let mut configured = Vec::new();
        for cells in pins.chunks(4) {
            let config = pin_config_from_cells(cells)?;
            let pin = config.id;
            self.inner.set_config(config).map_err(|err| {
                OnProbeError::other(format!(
                    "failed to apply pinctrl [{}] pin {pin:?}: {err}",
                    node.name()
                ))
            })?;
            configured.push(pin);
        }

        Ok(configured)
    }
}

impl DriverGeneric for RockchipPinCtrl {
    fn name(&self) -> &str {
        DRIVER_NAME
    }
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let fdt = live_fdt()?;

    let grf_phandle = info
        .node
        .as_node()
        .get_property("rockchip,grf")
        .and_then(|prop| prop.get_u32())
        .map(Phandle::from)
        .ok_or_else(|| {
            OnProbeError::other(format!("[{}] has no rockchip,grf", info.node.name()))
        })?;
    let ioc = map_phandle_reg(&fdt, grf_phandle, "pinctrl rockchip,grf")?;

    let mut gpio_banks = Vec::new();
    for node in fdt.find_compatible(&["rockchip,gpio-bank"]) {
        if gpio_banks.len() == GPIO_BANK_COUNT {
            break;
        }
        gpio_banks.push(map_node_reg(node, "rockchip,gpio-bank")?);
    }
    if gpio_banks.len() != GPIO_BANK_COUNT {
        return Err(OnProbeError::other(format!(
            "RK3588 pinctrl requires {GPIO_BANK_COUNT} GPIO banks, found {}",
            gpio_banks.len()
        )));
    }

    let pinctrl = PinCtrl::new(SocType::Rk3588, ioc, &gpio_banks);
    plat_dev.register(RockchipPinCtrl::new(pinctrl));
    info!("Rockchip RK3588 pinctrl registered successfully");
    Ok(())
}

fn live_fdt() -> Result<Fdt, OnProbeError> {
    rdrive::with_fdt(Clone::clone).ok_or_else(|| OnProbeError::other("live FDT not found"))
}

fn map_phandle_reg(
    fdt: &Fdt,
    phandle: Phandle,
    context: &str,
) -> Result<NonNull<u8>, OnProbeError> {
    let node = fdt
        .get_by_phandle(phandle)
        .ok_or_else(|| OnProbeError::other(format!("{context} phandle {phandle:?} not found")))?;
    map_node_reg(node, context)
}

fn map_node_reg(node: NodeType<'_>, context: &str) -> Result<NonNull<u8>, OnProbeError> {
    let reg = node.regs().into_iter().next().ok_or_else(|| {
        OnProbeError::other(format!("[{}] has no reg for {context}", node.name()))
    })?;
    map_reg(reg)
}

fn map_reg(reg: RegFixed) -> Result<NonNull<u8>, OnProbeError> {
    let size = align_up_4k((reg.size.unwrap_or(0x1000) as usize).max(1));
    iomap(reg.address as usize, size)
}

fn fixed_regulator_active_value(node: &Node) -> bool {
    node.get_property("enable-active-low").is_none()
}

fn gpio_active_low(node: &Node) -> bool {
    node.get_property("gpios")
        .or_else(|| node.get_property("gpio"))
        .and_then(|prop| prop.get_u32_iter().nth(2))
        .is_some_and(|flags| flags & 1 != 0)
}

fn parse_gpio_pin(fdt: &Fdt, node: &Node, prop_name: &str) -> Option<Result<PinId, OnProbeError>> {
    let prop = node.get_property(prop_name)?;
    Some((|| {
        let mut cells = prop.get_u32_iter();
        let phandle = Phandle::from(cells.next().ok_or_else(|| {
            OnProbeError::other(format!("[{}] has malformed {prop_name}", node.name()))
        })?);
        let pin = cells.next().ok_or_else(|| {
            OnProbeError::other(format!("[{}] has malformed {prop_name}", node.name()))
        })?;
        let gpio = fdt.get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] {prop_name} GPIO phandle {phandle:?} not found",
                node.name()
            ))
        })?;
        let bank = gpio_bank_index(gpio.as_node()).ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] cannot resolve GPIO bank for {prop_name} phandle {phandle:?}",
                node.name()
            ))
        })?;
        PinId::from_bank_pin(BankId::new(bank).unwrap_or(BankId::from(bank)), pin).ok_or_else(
            || {
                OnProbeError::other(format!(
                    "[{}] invalid GPIO bank {bank} pin {pin}",
                    node.name()
                ))
            },
        )
    })())
}

fn pin_config_from_cells(cells: &[u32]) -> Result<PinConfig, OnProbeError> {
    let [bank, pin, mux, conf_phandle] = cells else {
        return Err(OnProbeError::other("malformed rockchip,pins cells"));
    };
    let id = PinId::from_bank_pin(BankId::new(*bank).unwrap_or(BankId::from(*bank)), *pin)
        .ok_or_else(|| OnProbeError::other(format!("invalid Rockchip pin {bank}:{pin}")))?;
    let fdt = live_fdt()?;
    let conf = fdt
        .get_by_phandle(Phandle::from(*conf_phandle))
        .ok_or_else(|| {
            OnProbeError::other(format!("pinconf phandle {conf_phandle:?} not found"))
        })?;
    let mut pull = Pull::Disabled;
    let mut drive = None;
    for prop in conf.as_node().properties() {
        match prop.name() {
            "bias-disable" => pull = Pull::Disabled,
            "bias-bus-hold" => pull = Pull::BusHold,
            "bias-pull-up" => pull = Pull::PullUp,
            "bias-pull-down" => pull = Pull::PullDown,
            "bias-pull-pin-default" => pull = Pull::PullPinDefault,
            "drive-strength" => drive = prop.get_u32(),
            "phandle" => {}
            name => warn!("Unknown pinconf property: {}", name),
        }
    }
    Ok(PinConfig {
        id,
        mux: Iomux::from_bits_truncate(*mux as u8),
        pull,
        drive,
    })
}

fn gpio_bank_index(node: &Node) -> Option<u32> {
    let name = node.name();
    if let Some(name) = name
        .strip_prefix("gpio")
        .filter(|name| !name.starts_with('@'))
        && let Some(bank) = name
            .chars()
            .next()
            .and_then(|ch| ch.to_digit(10))
            .filter(|bank| *bank < GPIO_BANK_COUNT as u32)
    {
        return Some(bank);
    }

    let address = gpio_bank_address(node)?;
    match address {
        0xfd8a_0000 => Some(0),
        0xfec2_0000 => Some(1),
        0xfec3_0000 => Some(2),
        0xfec4_0000 => Some(3),
        0xfec5_0000 => Some(4),
        _ => None,
    }
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

fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}
