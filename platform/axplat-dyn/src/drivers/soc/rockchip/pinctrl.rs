extern crate alloc;

use alloc::{format, string::ToString, vec::Vec};
use core::ptr::NonNull;

use fdt_edit::{Fdt, NodeType, Phandle, RegFixed};
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo,
};
use rockchip_soc::{GpioDirection, PinConfig, PinCtrl, PinCtrlOp, PinId, SocType};

use crate::drivers::iomap;

const DRIVER_NAME: &str = "rk3588-pinctrl";
const GPIO_BANK_COUNT: usize = 5;

module_driver!(
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

pub(crate) struct RockchipPinCtrl {
    inner: PinCtrl,
    fdt_addr: NonNull<u8>,
}

unsafe impl Send for RockchipPinCtrl {}

impl RockchipPinCtrl {
    fn new(inner: PinCtrl, fdt_addr: NonNull<u8>) -> Self {
        Self { inner, fdt_addr }
    }

    pub(crate) fn enable_fixed_regulator(&mut self, phandle: Phandle) -> Result<(), OnProbeError> {
        let fdt = live_fdt(self.fdt_addr)?;
        let node = fdt.get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!("regulator phandle {phandle:?} not found"))
        })?;
        let node_name = node.name().to_string();
        let active_value = node.as_node().get_property("enable-active-low").is_none();
        let pinctrls = node
            .as_node()
            .get_property("pinctrl-0")
            .ok_or_else(|| OnProbeError::other(format!("[{node_name}] has no pinctrl-0")))?
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

        for pin in pins {
            self.inner
                .set_gpio_direction(pin, GpioDirection::Output(active_value))
                .map_err(|err| {
                    OnProbeError::other(format!(
                        "failed to set [{node_name}] GPIO {pin:?} direction: {err}"
                    ))
                })?;
            self.inner.write_gpio(pin, active_value).map_err(|err| {
                OnProbeError::other(format!("failed to drive [{node_name}] GPIO {pin:?}: {err}"))
            })?;
        }

        info!("Rockchip fixed regulator {node_name} enabled via pinctrl");
        Ok(())
    }

    fn apply_pinctrl_phandle(&mut self, phandle: Phandle) -> Result<Vec<PinId>, OnProbeError> {
        let fdt = live_fdt(self.fdt_addr)?;
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
            let config = PinConfig::new_with_fdt(cells, self.fdt_addr);
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

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let fdt_addr = live_fdt_addr()?;
    let fdt = live_fdt(fdt_addr)?;

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
    plat_dev.register(RockchipPinCtrl::new(pinctrl, fdt_addr));
    info!("Rockchip RK3588 pinctrl registered successfully");
    Ok(())
}

fn live_fdt_addr() -> Result<NonNull<u8>, OnProbeError> {
    let ptr = somehal::fdt_addr().ok_or_else(|| OnProbeError::other("live FDT not found"))?;
    NonNull::new(ptr).ok_or_else(|| OnProbeError::other("live FDT pointer is null"))
}

fn live_fdt(fdt_addr: NonNull<u8>) -> Result<Fdt, OnProbeError> {
    unsafe { Fdt::from_ptr(fdt_addr.as_ptr()) }
        .map_err(|err| OnProbeError::other(format!("failed to parse live FDT: {err:?}")))
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
    iomap((reg.address as usize).into(), size)
}

fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}
