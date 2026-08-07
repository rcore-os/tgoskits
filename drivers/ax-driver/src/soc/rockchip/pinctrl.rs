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

pub use rdif_glue::RockchipFdtPinctrlParser;
use rdif_glue::{ROCKCHIP_PIN_CONFIG_DRIVE_RAW, gpio_bank_index};

const GPIO_BANK_COUNT: usize = 5;
const GPIO_LINES_PER_BANK: u32 = 32;
const ROCKCHIP_GPIO_RANGES: [GpioRange; GPIO_BANK_COUNT] = [
    GpioRange::new(GpioBankId::new(0), 0, 0, GPIO_LINES_PER_BANK),
    GpioRange::new(GpioBankId::new(1), 32, 0, GPIO_LINES_PER_BANK),
    GpioRange::new(GpioBankId::new(2), 64, 0, GPIO_LINES_PER_BANK),
    GpioRange::new(GpioBankId::new(3), 96, 0, GPIO_LINES_PER_BANK),
    GpioRange::new(GpioBankId::new(4), 128, 0, GPIO_LINES_PER_BANK),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RockchipPinctrlVariant {
    Rk3576,
    Rk3588,
}

impl RockchipPinctrlVariant {
    fn from_compatible(compatible: &str) -> Option<Self> {
        match compatible {
            "rockchip,rk3576-pinctrl" => Some(Self::Rk3576),
            "rockchip,rk3588-pinctrl" => Some(Self::Rk3588),
            _ => None,
        }
    }

    const fn driver_name(self) -> &'static str {
        match self {
            Self::Rk3576 => "rk3576-pinctrl",
            Self::Rk3588 => "rk3588-pinctrl",
        }
    }

    const fn display_name(self) -> &'static str {
        match self {
            Self::Rk3576 => "RK3576",
            Self::Rk3588 => "RK3588",
        }
    }
}

crate::model_register!(
    name: "Rockchip PinCtrl",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3576-pinctrl", "rockchip,rk3588-pinctrl"],
            on_probe: probe
        }
    ],
);

pub struct RockchipPinCtrl {
    inner: PinCtrl,
    driver_name: &'static str,
}

unsafe impl Send for RockchipPinCtrl {}

impl RockchipPinCtrl {
    fn new(inner: PinCtrl, driver_name: &'static str) -> Self {
        Self { inner, driver_name }
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
        self.driver_name
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
            rdif_pinctrl::PinConfig::Bias(bias) => self
                .inner
                .set_pull(pin, rockchip_pull_from_rdif_bias(bias))
                .map_err(|_| RdifPinctrlError::InvalidConfig),
            rdif_pinctrl::PinConfig::Vendor { param, value }
                if param == ROCKCHIP_PIN_CONFIG_DRIVE_RAW =>
            {
                self.inner
                    .set_drive(pin, value)
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

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let variant = info
        .node
        .as_node()
        .compatibles()
        .find_map(RockchipPinctrlVariant::from_compatible)
        .ok_or(OnProbeError::NotMatch)?;
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

    let mut gpio_banks = [None; GPIO_BANK_COUNT];
    for node in fdt.find_compatible(&["rockchip,gpio-bank"]) {
        let Some(bank) = gpio_bank_index(node.as_node()) else {
            continue;
        };
        let bank = bank as usize;
        if gpio_banks[bank].is_some() {
            return Err(OnProbeError::other(format!(
                "{} pinctrl has duplicate GPIO bank {bank}",
                variant.display_name()
            )));
        }
        gpio_banks[bank] = Some(map_node_reg(node, "rockchip,gpio-bank")?);
    }
    let gpio_banks = gpio_banks
        .into_iter()
        .enumerate()
        .map(|(bank, mapped)| {
            mapped.ok_or_else(|| {
                OnProbeError::other(format!(
                    "{} pinctrl is missing GPIO bank {bank}",
                    variant.display_name()
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let pinctrl = match variant {
        RockchipPinctrlVariant::Rk3576 => {
            let sys_grf = info
                .node
                .as_node()
                .get_property("rockchip,sys-grf")
                .and_then(|prop| prop.get_u32())
                .map(Phandle::from)
                .map(|phandle| map_phandle_reg(&fdt, phandle, "pinctrl rockchip,sys-grf"))
                .transpose()?;
            PinCtrl::new_rk3576(ioc, sys_grf, &gpio_banks)
        }
        RockchipPinctrlVariant::Rk3588 => PinCtrl::new(SocType::Rk3588, ioc, &gpio_banks),
    };
    plat_dev.register(PinctrlDevice::with_fdt_parser(
        RockchipPinCtrl::new(pinctrl, variant.driver_name()),
        RockchipFdtPinctrlParser,
    ));
    info!(
        "Rockchip {} pinctrl registered successfully",
        variant.display_name()
    );
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

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use fdt_edit::{Node, Property};

    use super::*;

    fn mmio(words: &mut [u32]) -> NonNull<u8> {
        NonNull::new(words.as_mut_ptr().cast()).unwrap()
    }

    fn prop_u32s(name: &str, values: &[u32]) -> Property {
        Property::new(
            name,
            values
                .iter()
                .flat_map(|value| value.to_be_bytes())
                .collect(),
        )
    }

    fn prop_strs(name: &str, values: &[&str]) -> Property {
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        Property::new(name, bytes)
    }

    fn node_with_props(name: &str, properties: &[Property]) -> Node {
        let mut node = Node::new(name);
        for property in properties {
            node.add_property(property.clone());
        }
        node
    }

    #[test]
    fn selects_rk3576_pinctrl_from_fdt_compatible() {
        let variant = RockchipPinctrlVariant::from_compatible("rockchip,rk3576-pinctrl").unwrap();

        assert_eq!(variant, RockchipPinctrlVariant::Rk3576);
        assert_eq!(variant.driver_name(), "rk3576-pinctrl");
        assert_eq!(variant.display_name(), "RK3576");
    }

    #[test]
    fn preserves_rk3588_pinctrl_selection() {
        let variant = RockchipPinctrlVariant::from_compatible("rockchip,rk3588-pinctrl").unwrap();

        assert_eq!(variant, RockchipPinctrlVariant::Rk3588);
        assert_eq!(variant.driver_name(), "rk3588-pinctrl");
    }

    #[test]
    fn rejects_unknown_rockchip_pinctrl_compatible() {
        assert!(RockchipPinctrlVariant::from_compatible("rockchip,rk3568-pinctrl").is_none());
    }

    #[test]
    fn rock_4d_sdmmc_default_state_programs_rk3576_ioc() {
        let mut ioc_memory = vec![0_u32; (0xb398 + 4) / 4];
        let mut gpio_memory: Vec<Vec<u32>> = (0..5).map(|_| vec![0_u32; 0x200 / 4]).collect();
        let gpio_banks = gpio_memory
            .iter_mut()
            .map(|bank| mmio(bank))
            .collect::<Vec<_>>();
        let pinctrl = PinCtrl::new_rk3576(mmio(&mut ioc_memory), None, &gpio_banks);
        let mut controller = RockchipPinCtrl::new(pinctrl, "rk3576-pinctrl");

        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            node_with_props(
                "pcfg-pull-up-drv-level-3",
                &[
                    prop_u32s("phandle", &[1]),
                    Property::new("bias-pull-up", Vec::new()),
                    prop_u32s("drive-strength", &[3]),
                ],
            ),
        );
        fdt.add_node(
            root,
            node_with_props(
                "sdmmc0-pins",
                &[
                    prop_u32s("phandle", &[2]),
                    prop_u32s(
                        "rockchip,pins",
                        &[
                            2, 0, 1, 1, // GPIO2_A0: data
                            2, 5, 1, 1, // GPIO2_A5: clock
                            0, 7, 1, 1, // GPIO0_A7: card detect
                            0, 14, 1, 1, // GPIO0_B6: power enable
                        ],
                    ),
                ],
            ),
        );
        let consumer = fdt.add_node(
            root,
            node_with_props(
                "mmc@2a310000",
                &[
                    prop_strs("pinctrl-names", &["default"]),
                    prop_u32s("pinctrl-0", &[2]),
                ],
            ),
        );

        FdtPinctrl::apply_state_from_consumer(
            &mut controller,
            &fdt,
            fdt.node(consumer).unwrap(),
            0,
            &RockchipFdtPinctrlParser,
        )
        .unwrap();

        assert_eq!(ioc_memory[0x4040 / 4], 0x000f_0001);
        assert_eq!(ioc_memory[0x4044 / 4], 0x00f0_0010);
        assert_eq!(ioc_memory[0x0004 / 4], 0xf000_1000);
        assert_eq!(ioc_memory[0x2000 / 4], 0x0f00_0100);
        assert_eq!(ioc_memory[0x6120 / 4], 0x0c00_0c00);
        assert_eq!(ioc_memory[0x6044 / 4], 0x00f0_0060);
    }

    #[test]
    fn rk3576_fixed_gpio_regulator_applies_pinctrl_and_drives_enable() {
        let mut ioc_memory = vec![0_u32; (0xb398 + 4) / 4];
        let mut gpio_memory: Vec<Vec<u32>> = (0..5).map(|_| vec![0_u32; 0x200 / 4]).collect();
        let gpio_banks = gpio_memory
            .iter_mut()
            .map(|bank| mmio(bank))
            .collect::<Vec<_>>();
        let pinctrl = PinCtrl::new_rk3576(mmio(&mut ioc_memory), None, &gpio_banks);
        let mut controller = RockchipPinCtrl::new(pinctrl, "rk3576-pinctrl");

        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            node_with_props(
                "gpio@2ae20000",
                &[
                    prop_u32s("phandle", &[30]),
                    prop_strs("compatible", &["rockchip,gpio-bank"]),
                    prop_u32s("gpio-ranges", &[40, 0, 64, 32]),
                ],
            ),
        );
        fdt.add_node(
            root,
            node_with_props(
                "pcfg-pull-none",
                &[
                    prop_u32s("phandle", &[31]),
                    Property::new("bias-disable", Vec::new()),
                ],
            ),
        );
        fdt.add_node(
            root,
            node_with_props(
                "sd-enable-pin",
                &[
                    prop_u32s("phandle", &[32]),
                    prop_u32s("rockchip,pins", &[2, 7, 0, 31]),
                ],
            ),
        );
        let regulator = fdt.add_node(
            root,
            node_with_props(
                "vcc3v3-sd",
                &[
                    prop_strs("compatible", &["regulator-fixed"]),
                    prop_strs("pinctrl-names", &["default"]),
                    prop_u32s("pinctrl-0", &[32]),
                    prop_u32s("gpios", &[30, 7, 0]),
                ],
            ),
        );

        FdtPinctrl::apply_fixed_regulator(
            &mut controller,
            &fdt,
            fdt.node(regulator).unwrap(),
            &RockchipFdtPinctrlParser,
            "test-sd-regulator",
        )
        .unwrap();

        assert_eq!(ioc_memory[0x4044 / 4], 0xf000_0000);
        assert_eq!(gpio_memory[2][0x00 / 4], 0xffff_0080);
        assert_eq!(gpio_memory[2][0x08 / 4], 0xffff_0080);
    }
}
