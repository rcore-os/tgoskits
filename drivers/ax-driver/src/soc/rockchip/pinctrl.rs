extern crate alloc;

use alloc::{format, string::ToString, vec::Vec};
use core::ptr::NonNull;

use fdt_edit::{Fdt, NodeType, Phandle, RegFixed};
use log::info;
use rdif_pinctrl::{
    Bias, ConfigSetting, ConfigTarget, FdtPinctrl, FunctionId, GpioBankId, GpioRange, GroupId,
    Interface as RdifPinctrl, MuxSetting, PinId as RdifPinId, PinState, PinctrlDevice,
    PinctrlError as RdifPinctrlError,
};
use rdrive::{DriverGeneric, probe::OnProbeError, register::ProbeFdt};
use rockchip_soc::{
    GpioDirection, Iomux, PinConfig as RockchipPinConfig, PinCtrl, PinCtrlOp,
    PinId as RockchipPinId, Pull, SocType,
};

use crate::mmio::iomap;

mod rdif_glue;

use rdif_glue::ROCKCHIP_PIN_CONFIG_DRIVE_RAW;
pub use rdif_glue::RockchipFdtPinctrlParser;

const DRIVER_NAME: &str = "rk3588-pinctrl";
const GPIO_BANK_COUNT: usize = 5;
const GPIO_LINES_PER_BANK: u32 = 32;
const ROCKCHIP_GPIO_RANGES: [GpioRange; GPIO_BANK_COUNT] = [
    GpioRange::new(GpioBankId::new(0), 0, 0, GPIO_LINES_PER_BANK),
    GpioRange::new(GpioBankId::new(1), 32, 0, GPIO_LINES_PER_BANK),
    GpioRange::new(GpioBankId::new(2), 64, 0, GPIO_LINES_PER_BANK),
    GpioRange::new(GpioBankId::new(3), 96, 0, GPIO_LINES_PER_BANK),
    GpioRange::new(GpioBankId::new(4), 128, 0, GPIO_LINES_PER_BANK),
];

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

    pub fn enable_fixed_regulator(&mut self, phandle: Phandle) -> Result<(), OnProbeError> {
        let fdt = live_fdt()?;
        let node = fdt.get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!("regulator phandle {phandle:?} not found"))
        })?;
        let node_name = node.name().to_string();
        FdtPinctrl::apply_fixed_regulator(
            self,
            &fdt,
            node.as_node(),
            &RockchipFdtPinctrlParser,
            "rockchip-fixed-regulator",
        )
        .map_err(|err| {
            OnProbeError::other(format!(
                "failed to apply fixed regulator [{node_name}] via RDIF pinctrl: {err}"
            ))
        })?;

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
}

impl DriverGeneric for RockchipPinCtrl {
    fn name(&self) -> &str {
        DRIVER_NAME
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

impl RdifPinctrl for RockchipPinCtrl {
    fn gpio_ranges(&self) -> &[GpioRange] {
        &ROCKCHIP_GPIO_RANGES
    }

    fn can_mux(&self, group: GroupId, function: FunctionId) -> bool {
        rockchip_pin_id(group.raw()).is_ok() && function.raw() <= 0xff
    }

    fn validate_state(&self, state: &PinState) -> Result<(), RdifPinctrlError> {
        for mux in state.muxes() {
            if rockchip_pin_id(mux.group.raw()).is_err() {
                return Err(RdifPinctrlError::InvalidGroup(mux.group));
            }
            if !self.can_mux(mux.group, mux.function) {
                return Err(RdifPinctrlError::InvalidMux {
                    group: mux.group,
                    function: mux.function,
                });
            }
        }

        for config in state.configs() {
            match config.target {
                ConfigTarget::Pin(pin) => {
                    if rockchip_pin_id(pin.raw()).is_err() {
                        return Err(RdifPinctrlError::InvalidPin(pin));
                    }
                }
                ConfigTarget::Group(group) => {
                    if rockchip_pin_id(group.raw()).is_err() {
                        return Err(RdifPinctrlError::InvalidGroup(group));
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_mux(&mut self, setting: &MuxSetting) -> Result<(), RdifPinctrlError> {
        let pin = rockchip_pin_id(setting.group.raw())?;
        let mut config = self.inner.get_config(pin).unwrap_or(RockchipPinConfig {
            id: pin,
            mux: Iomux::from_bits_truncate(0),
            pull: Pull::Disabled,
            drive: None,
        });
        config.mux = Iomux::from_bits_truncate(setting.value.raw() as u8);
        self.inner
            .set_config(config)
            .map_err(|_| RdifPinctrlError::InvalidConfig)
    }

    fn apply_config(&mut self, setting: &ConfigSetting) -> Result<(), RdifPinctrlError> {
        let pin = match setting.target {
            ConfigTarget::Pin(pin) => rockchip_pin_id(pin.raw())?,
            ConfigTarget::Group(group) => rockchip_pin_id(group.raw())?,
        };

        match setting.config {
            rdif_pinctrl::PinConfig::Bias(bias) => {
                let mut config = self.current_or_default_config(pin);
                config.pull = rockchip_pull_from_rdif_bias(bias);
                self.inner
                    .set_config(config)
                    .map_err(|_| RdifPinctrlError::InvalidConfig)
            }
            rdif_pinctrl::PinConfig::Vendor { param, value }
                if param == ROCKCHIP_PIN_CONFIG_DRIVE_RAW =>
            {
                let mut config = self.current_or_default_config(pin);
                config.drive = Some(value);
                self.inner
                    .set_config(config)
                    .map_err(|_| RdifPinctrlError::InvalidConfig)
            }
            rdif_pinctrl::PinConfig::InputEnable(true) => self
                .inner
                .set_gpio_direction(pin, GpioDirection::Input)
                .map_err(|_| RdifPinctrlError::InvalidConfig),
            rdif_pinctrl::PinConfig::OutputEnable(true) => {
                let value = self.inner.read_gpio(pin).unwrap_or(false);
                self.inner
                    .set_gpio_direction(pin, GpioDirection::Output(value))
                    .map_err(|_| RdifPinctrlError::InvalidConfig)
            }
            rdif_pinctrl::PinConfig::OutputValue(value) => self
                .inner
                .write_gpio(pin, value)
                .map_err(|_| RdifPinctrlError::InvalidConfig),
            rdif_pinctrl::PinConfig::InputEnable(false)
            | rdif_pinctrl::PinConfig::DriveStrengthUa(_)
            | rdif_pinctrl::PinConfig::OutputEnable(false)
            | rdif_pinctrl::PinConfig::SlewRate(_)
            | rdif_pinctrl::PinConfig::DebounceUs(_)
            | rdif_pinctrl::PinConfig::LowPowerMode(_)
            | rdif_pinctrl::PinConfig::Vendor { .. } => Err(RdifPinctrlError::NotSupported),
        }
    }
}

impl RockchipPinCtrl {
    fn current_or_default_config(&self, pin: RockchipPinId) -> RockchipPinConfig {
        self.inner.get_config(pin).unwrap_or(RockchipPinConfig {
            id: pin,
            mux: Iomux::from_bits_truncate(0),
            pull: Pull::Disabled,
            drive: None,
        })
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
    plat_dev.register(PinctrlDevice::with_fdt_parser(
        RockchipPinCtrl::new(pinctrl),
        RockchipFdtPinctrlParser,
    ));
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

fn rockchip_pin_id(raw_pin: u32) -> Result<RockchipPinId, RdifPinctrlError> {
    RockchipPinId::new(raw_pin).ok_or_else(|| RdifPinctrlError::InvalidPin(RdifPinId::new(raw_pin)))
}

fn rockchip_pull_from_rdif_bias(bias: Bias) -> Pull {
    match bias {
        Bias::Disabled => Pull::Disabled,
        Bias::BusHold => Pull::BusHold,
        Bias::PullUp => Pull::PullUp,
        Bias::PullDown => Pull::PullDown,
        Bias::PullPinDefault => Pull::PullPinDefault,
    }
}

fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}
