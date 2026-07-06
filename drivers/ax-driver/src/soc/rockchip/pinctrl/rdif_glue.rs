extern crate alloc;

use alloc::{format, vec::Vec};

use fdt_edit::{Fdt, Node, NodeType, Phandle};
use log::warn;
use rdif_pinctrl::{
    Bias, ConfigSetting, FdtPinctrlParser, FunctionId, GpioBankId, GpioLineId, GroupId, MuxSetting,
    MuxValue, PinConfig as RdifPinConfig, PinId as RdifPinId, PinState, PinctrlError,
};
use rockchip_soc::{BankId, Iomux, PinConfig as RockchipPinConfig, PinId as RockchipPinId, Pull};

use super::{GPIO_BANK_COUNT, GPIO_LINES_PER_BANK};

pub struct RockchipFdtPinctrlParser;

pub(super) const ROCKCHIP_PIN_CONFIG_DRIVE_RAW: u32 = 1;

impl FdtPinctrlParser for RockchipFdtPinctrlParser {
    fn parse_pinctrl_node(
        &self,
        fdt: &Fdt,
        node: NodeType<'_>,
        state: &mut PinState,
    ) -> Result<(), PinctrlError> {
        append_rockchip_node_to_state(fdt, node, state)
    }

    fn parse_gpio_line(
        &self,
        fdt: &Fdt,
        node: &Node,
        prop_name: &str,
    ) -> Option<Result<GpioLineId, PinctrlError>> {
        parse_gpio_line(fdt, node, prop_name)
    }

    fn gpio_lines_from_state(&self, state: &PinState) -> Result<Vec<GpioLineId>, PinctrlError> {
        state
            .muxes()
            .iter()
            .map(|mux| rdif_gpio_line_from_raw_pin(mux.group.raw()))
            .collect()
    }
}

fn parse_gpio_line(
    fdt: &Fdt,
    node: &Node,
    prop_name: &str,
) -> Option<Result<GpioLineId, PinctrlError>> {
    let prop = node.get_property(prop_name)?;
    Some((|| {
        let mut cells = prop.get_u32_iter();
        let phandle = Phandle::from(cells.next().ok_or_else(|| {
            PinctrlError::other(format!("[{}] has malformed {prop_name}", node.name()))
        })?);
        let pin = cells.next().ok_or_else(|| {
            PinctrlError::other(format!("[{}] has malformed {prop_name}", node.name()))
        })?;
        let gpio = fdt.get_by_phandle(phandle).ok_or_else(|| {
            PinctrlError::other(format!(
                "[{}] {prop_name} GPIO phandle {phandle:?} not found",
                node.name()
            ))
        })?;
        let bank = gpio_bank_index(gpio.as_node()).ok_or_else(|| {
            PinctrlError::other(format!(
                "[{}] cannot resolve GPIO bank for {prop_name} phandle {phandle:?}",
                node.name()
            ))
        })?;
        if bank >= GPIO_BANK_COUNT as u32 || pin >= GPIO_LINES_PER_BANK {
            return Err(PinctrlError::other(format!(
                "[{}] invalid GPIO bank {bank} pin {pin}",
                node.name()
            )));
        }
        Ok(GpioLineId::new(GpioBankId::new(bank), pin))
    })())
}

fn pin_config_from_cells_with_fdt(
    fdt: &Fdt,
    cells: &[u32],
) -> Result<RockchipPinConfig, PinctrlError> {
    let [bank, pin, mux, conf_phandle] = cells else {
        return Err(PinctrlError::other("malformed rockchip,pins cells"));
    };
    let id = RockchipPinId::from_bank_pin(BankId::new(*bank).unwrap_or(BankId::from(*bank)), *pin)
        .ok_or_else(|| PinctrlError::other(format!("invalid Rockchip pin {bank}:{pin}")))?;
    let conf = fdt
        .get_by_phandle(Phandle::from(*conf_phandle))
        .ok_or_else(|| {
            PinctrlError::other(format!("pinconf phandle {conf_phandle:?} not found"))
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
    Ok(RockchipPinConfig {
        id,
        mux: Iomux::from_bits_truncate(*mux as u8),
        pull,
        drive,
    })
}

fn append_rockchip_node_to_state(
    fdt: &Fdt,
    node: NodeType<'_>,
    state: &mut PinState,
) -> Result<(), PinctrlError> {
    let pins = node
        .as_node()
        .get_property("rockchip,pins")
        .ok_or_else(|| PinctrlError::other(format!("[{}] has no rockchip,pins", node.name())))?
        .get_u32_iter()
        .collect::<Vec<_>>();
    if pins.len() % 4 != 0 {
        return Err(PinctrlError::other(format!(
            "[{}] has malformed rockchip,pins with {} cells",
            node.name(),
            pins.len()
        )));
    }

    for cells in pins.chunks(4) {
        let [bank, pin, mux, _conf_phandle] = cells else {
            unreachable!("chunks were prevalidated");
        };
        let raw_pin = rockchip_raw_pin(*bank, *pin)?;
        let config = pin_config_from_cells_with_fdt(fdt, cells)?;
        let group = GroupId::new(raw_pin);
        state.push_mux(MuxSetting::new(
            group,
            FunctionId::new(*mux),
            MuxValue::new(*mux),
        ));
        state.push_config(ConfigSetting::pin(
            RdifPinId::new(raw_pin),
            RdifPinConfig::Bias(rdif_bias_from_rockchip_pull(config.pull)),
        ));
        if let Some(drive) = config.drive {
            state.push_config(ConfigSetting::pin(
                RdifPinId::new(raw_pin),
                RdifPinConfig::Vendor {
                    param: ROCKCHIP_PIN_CONFIG_DRIVE_RAW,
                    value: drive,
                },
            ));
        }
    }
    Ok(())
}

fn rockchip_raw_pin(bank: u32, pin: u32) -> Result<u32, PinctrlError> {
    if bank >= GPIO_BANK_COUNT as u32 || pin >= GPIO_LINES_PER_BANK {
        return Err(PinctrlError::other(format!(
            "invalid Rockchip pin {bank}:{pin}"
        )));
    }
    Ok(bank * GPIO_LINES_PER_BANK + pin)
}

fn rdif_gpio_line_from_raw_pin(raw_pin: u32) -> Result<GpioLineId, PinctrlError> {
    let bank = raw_pin / GPIO_LINES_PER_BANK;
    let offset = raw_pin % GPIO_LINES_PER_BANK;
    if bank >= GPIO_BANK_COUNT as u32 {
        return Err(PinctrlError::other(format!(
            "invalid Rockchip raw GPIO pin {raw_pin}"
        )));
    }
    Ok(GpioLineId::new(GpioBankId::new(bank), offset))
}

fn rdif_bias_from_rockchip_pull(pull: Pull) -> Bias {
    match pull {
        Pull::Disabled => Bias::Disabled,
        Pull::BusHold => Bias::BusHold,
        Pull::PullUp => Bias::PullUp,
        Pull::PullDown => Bias::PullDown,
        Pull::PullPinDefault => Bias::PullPinDefault,
    }
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

#[cfg(test)]
mod tests {
    use alloc::vec;

    #[cfg(not(feature = "pci"))]
    use axklib::{
        AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle,
        IrqId, Klib, PhysAddr, VirtAddr, impl_trait,
    };
    use fdt_edit::{Node, Property};
    use rdif_pinctrl::{FdtPinctrl, StateName};

    use super::*;

    #[cfg(not(feature = "pci"))]
    struct KlibImpl;

    #[cfg(not(feature = "pci"))]
    impl_trait! {
        impl Klib for KlibImpl {
            fn mem_iomap(_addr: PhysAddr, _size: usize) -> AxResult<VirtAddr> {
                Err(AxError::Unsupported)
            }

            fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
                PhysAddr::from_usize(addr.as_usize())
            }

            fn mem_make_dma_coherent_uncached(_addr: VirtAddr, _size: usize) -> AxResult {
                Err(AxError::Unsupported)
            }

            fn mem_restore_dma_cached(_addr: VirtAddr, _size: usize) -> AxResult {
                Err(AxError::Unsupported)
            }

            fn dma_alloc_pages(
                _dma_mask: u64,
                _num_pages: usize,
                _align: usize,
            ) -> AxResult<VirtAddr> {
                Err(AxError::Unsupported)
            }

            fn dma_dealloc_pages(_addr: VirtAddr, _num_pages: usize) {}

            fn time_busy_wait(_dur: core::time::Duration) {}

            fn time_monotonic_nanos() -> u64 {
                0
            }

            fn time_try_init_epoch_offset(_epoch_time_nanos: u64) -> bool {
                false
            }

            fn irq_set_enable(_irq: IrqId, _enabled: bool) -> axklib::AxResult {
                Ok(())
            }

            fn irq_request_shared(
                _irq: IrqId,
                _handler: BoxedIrqHandler,
            ) -> AxResult<IrqHandle> {
                Err(AxError::Unsupported)
            }

            fn irq_request_shared_disabled(
                _irq: IrqId,
                _handler: BoxedIrqHandler,
            ) -> AxResult<IrqHandle> {
                Err(AxError::Unsupported)
            }

            fn irq_request_percpu(
                _irq: IrqId,
                _cpus: IrqCpuMask,
                _handler: ConcurrentBoxedIrqHandler,
            ) -> AxResult<IrqHandle> {
                Err(AxError::Unsupported)
            }

            fn irq_free(_handle: IrqHandle) -> axklib::AxResult {
                Ok(())
            }

            fn irq_enable(_handle: IrqHandle) -> axklib::AxResult {
                Ok(())
            }

            fn irq_disable(_handle: IrqHandle) -> axklib::AxResult {
                Ok(())
            }
        }
    }

    #[test]
    fn rockchip_pins_state_preserves_raw_mux_value() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            conf_node("pcfg-pull-up", 10, &["bias-pull-up"], Some(8000)),
        );
        fdt.add_node(
            root,
            node_with_props(
                "uart0-pins",
                &[
                    prop_u32s("phandle", &[20]),
                    prop_u32s("rockchip,pins", &[1, 2, 5, 10]),
                ],
            ),
        );

        let mut state = PinState::named(StateName::Default);
        RockchipFdtPinctrlParser
            .parse_pinctrl_node(
                &fdt,
                fdt.get_by_phandle(Phandle::from(20)).unwrap(),
                &mut state,
            )
            .unwrap();

        assert_eq!(state.muxes().len(), 1);
        assert_eq!(state.muxes()[0].group, GroupId::new(34));
        assert_eq!(state.muxes()[0].function, FunctionId::new(5));
        assert_eq!(state.muxes()[0].value.raw(), 5);
        assert!(state.configs().contains(&ConfigSetting::pin(
            RdifPinId::new(34),
            RdifPinConfig::Bias(Bias::PullUp)
        )));
        assert!(state.configs().contains(&ConfigSetting::pin(
            RdifPinId::new(34),
            RdifPinConfig::Vendor {
                param: ROCKCHIP_PIN_CONFIG_DRIVE_RAW,
                value: 8000,
            }
        )));
    }

    #[test]
    fn pinctrl_0_multiple_phandles_merge_into_one_state() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            conf_node("pcfg-disabled", 10, &["bias-disable"], None),
        );
        fdt.add_node(
            root,
            node_with_props(
                "uart0-tx",
                &[
                    prop_u32s("phandle", &[21]),
                    prop_u32s("rockchip,pins", &[1, 3, 10, 10]),
                ],
            ),
        );
        fdt.add_node(
            root,
            node_with_props(
                "uart0-rx",
                &[
                    prop_u32s("phandle", &[22]),
                    prop_u32s("rockchip,pins", &[1, 4, 11, 10]),
                ],
            ),
        );
        let consumer = fdt.add_node(
            root,
            node_with_props(
                "serial@feb50000",
                &[
                    prop_strs("pinctrl-names", &["default"]),
                    prop_u32s("pinctrl-0", &[21, 22]),
                ],
            ),
        );

        let state = FdtPinctrl::state_from_consumer(
            &fdt,
            fdt.node(consumer).unwrap(),
            0,
            &RockchipFdtPinctrlParser,
        )
        .unwrap();

        assert_eq!(state.name(), &StateName::Default);
        assert_eq!(state.muxes().len(), 2);
        assert_eq!(state.muxes()[0].group, GroupId::new(35));
        assert_eq!(state.muxes()[0].value.raw(), 10);
        assert_eq!(state.muxes()[1].group, GroupId::new(36));
        assert_eq!(state.muxes()[1].value.raw(), 11);
    }

    #[test]
    fn fixed_regulator_gpio_and_pinctrl_paths_map_to_gpio_lines() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            node_with_props(
                "gpio2@fec30000",
                &[
                    prop_u32s("phandle", &[30]),
                    prop_strs("compatible", &["rockchip,gpio-bank"]),
                ],
            ),
        );
        fdt.add_node(root, conf_node("pcfg-gpio", 10, &["bias-disable"], None));
        fdt.add_node(
            root,
            node_with_props(
                "vcc3v3-pinctrl",
                &[
                    prop_u32s("phandle", &[31]),
                    prop_u32s("rockchip,pins", &[3, 4, 0, 10]),
                ],
            ),
        );
        let direct = fdt.add_node(
            root,
            node_with_props("regulator-direct", &[prop_u32s("gpios", &[30, 7, 0])]),
        );
        let legacy = fdt.add_node(
            root,
            node_with_props("regulator-legacy", &[prop_u32s("gpio", &[30, 8, 0])]),
        );
        let pinctrl = fdt.add_node(
            root,
            node_with_props("regulator-pinctrl", &[prop_u32s("pinctrl-0", &[31])]),
        );

        assert_eq!(
            FdtPinctrl::gpio_lines_from_node(
                &fdt,
                fdt.node(direct).unwrap(),
                &RockchipFdtPinctrlParser,
            )
            .unwrap(),
            vec![GpioLineId::new(GpioBankId::new(2), 7)]
        );
        assert_eq!(
            FdtPinctrl::gpio_lines_from_node(
                &fdt,
                fdt.node(legacy).unwrap(),
                &RockchipFdtPinctrlParser,
            )
            .unwrap(),
            vec![GpioLineId::new(GpioBankId::new(2), 8)]
        );
        assert_eq!(
            FdtPinctrl::gpio_lines_from_node(
                &fdt,
                fdt.node(pinctrl).unwrap(),
                &RockchipFdtPinctrlParser,
            )
            .unwrap(),
            vec![GpioLineId::new(GpioBankId::new(3), 4)]
        );
    }

    fn conf_node(name: &str, phandle: u32, biases: &[&str], drive: Option<u32>) -> Node {
        let mut node = node_with_props(name, &[prop_u32s("phandle", &[phandle])]);
        for bias in biases {
            node.set_property(Property::new(bias, Vec::new()));
        }
        if let Some(drive) = drive {
            node.set_property(prop_u32s("drive-strength", &[drive]));
        }
        node
    }

    fn node_with_props(name: &str, props: &[Property]) -> Node {
        let mut node = Node::new(name);
        for prop in props {
            node.set_property(prop.clone());
        }
        node
    }

    fn prop_u32s(name: &str, values: &[u32]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(&value.to_be_bytes());
        }
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
