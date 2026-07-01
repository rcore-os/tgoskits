use alloc::{format, string::String, vec, vec::Vec};

use fdt_edit::{Fdt, Node, NodeType, Phandle};

use crate::{
    ConfigSetting, Direction, FirmwareKind, GpioLineId, Interface, PinConfig, PinId, PinState,
    PinctrlError, StateName,
};

pub trait FdtPinctrlParser {
    fn parse_pinctrl_node(
        &self,
        fdt: &Fdt,
        node: NodeType<'_>,
        state: &mut PinState,
    ) -> Result<(), PinctrlError>;

    fn parse_gpio_line(
        &self,
        _fdt: &Fdt,
        _consumer: &Node,
        _prop_name: &str,
    ) -> Option<Result<GpioLineId, PinctrlError>> {
        None
    }

    fn gpio_lines_from_state(&self, _state: &PinState) -> Result<Vec<GpioLineId>, PinctrlError> {
        Ok(Vec::new())
    }
}

pub struct FdtPinctrl;

impl FdtPinctrl {
    pub const fn unsupported_firmware(kind: FirmwareKind) -> PinctrlError {
        PinctrlError::UnsupportedFirmware(kind)
    }

    pub fn state_from_consumer(
        fdt: &Fdt,
        node: &Node,
        index: u32,
        parser: &(impl FdtPinctrlParser + ?Sized),
    ) -> Result<PinState, PinctrlError> {
        let prop_name = format!("pinctrl-{index}");
        let prop = node
            .get_property(&prop_name)
            .ok_or_else(|| PinctrlError::other(format!("[{}] has no {prop_name}", node.name())))?;
        let mut state = PinState::named(Self::state_name_from_pinctrl_names(node, index));

        for phandle in prop.get_u32_iter().map(Phandle::from) {
            let pinctrl = fdt.get_by_phandle(phandle).ok_or_else(|| {
                PinctrlError::other(format!(
                    "[{}] {prop_name} phandle {phandle:?} not found",
                    node.name()
                ))
            })?;
            parser.parse_pinctrl_node(fdt, pinctrl, &mut state)?;
        }

        Ok(state)
    }

    pub fn apply_state_from_consumer(
        interface: &mut (impl Interface + ?Sized),
        fdt: &Fdt,
        node: &Node,
        index: u32,
        parser: &(impl FdtPinctrlParser + ?Sized),
    ) -> Result<(), PinctrlError> {
        let state = Self::state_from_consumer(fdt, node, index, parser)?;
        interface.apply_state(&state)
    }

    pub fn gpio_lines_from_node(
        fdt: &Fdt,
        node: &Node,
        parser: &(impl FdtPinctrlParser + ?Sized),
    ) -> Result<Vec<GpioLineId>, PinctrlError> {
        if let Some(line) = parser
            .parse_gpio_line(fdt, node, "gpios")
            .or_else(|| parser.parse_gpio_line(fdt, node, "gpio"))
        {
            return line.map(|line| vec![line]);
        }

        if node.get_property("pinctrl-0").is_none() {
            return Ok(Vec::new());
        }

        let state = Self::state_from_consumer(fdt, node, 0, parser)?;
        parser.gpio_lines_from_state(&state)
    }

    pub fn apply_fixed_regulator(
        interface: &mut (impl Interface + ?Sized),
        fdt: &Fdt,
        regulator_node: &Node,
        parser: &(impl FdtPinctrlParser + ?Sized),
        owner: &str,
    ) -> Result<(), PinctrlError> {
        let state = if regulator_node.get_property("pinctrl-0").is_some() {
            let state = Self::state_from_consumer(fdt, regulator_node, 0, parser)?;
            interface.apply_state(&state)?;
            Some(state)
        } else {
            None
        };

        let lines = if let Some(line) = parser
            .parse_gpio_line(fdt, regulator_node, "gpios")
            .or_else(|| parser.parse_gpio_line(fdt, regulator_node, "gpio"))
        {
            vec![line?]
        } else if let Some(state) = &state {
            parser.gpio_lines_from_state(state)?
        } else {
            Vec::new()
        };

        if lines.is_empty() {
            return Err(PinctrlError::NotAvailable);
        }

        let active_value = regulator_node.get_property("enable-active-low").is_none();
        let gpio_active_low = Self::gpio_flags_active_low(regulator_node);
        let output_value = if gpio_active_low {
            !active_value
        } else {
            active_value
        };

        for line in lines {
            Self::drive_gpio_line(interface, line, output_value, owner)?;
        }

        Ok(())
    }

    fn state_name_from_pinctrl_names(node: &Node, index: u32) -> StateName {
        let Some(name) = node
            .get_property("pinctrl-names")
            .and_then(|prop| prop.as_str_iter().nth(index as usize))
        else {
            return if index == 0 {
                StateName::Default
            } else {
                StateName::Index(index)
            };
        };

        match name {
            "default" => StateName::Default,
            "init" => StateName::Init,
            "idle" => StateName::Idle,
            "sleep" => StateName::Sleep,
            other => StateName::Named(String::from(other)),
        }
    }

    fn gpio_flags_active_low(node: &Node) -> bool {
        node.get_property("gpios")
            .or_else(|| node.get_property("gpio"))
            .and_then(|prop| prop.get_u32_iter().nth(2))
            .is_some_and(|flags| flags & 1 != 0)
    }

    fn drive_gpio_line(
        interface: &mut (impl Interface + ?Sized),
        line: GpioLineId,
        value: bool,
        owner: &str,
    ) -> Result<(), PinctrlError> {
        if let Some(mut bank) = interface.create_gpio_bank(line.bank) {
            if line.offset >= bank.line_count() {
                return Err(PinctrlError::InvalidLine(line));
            }
            let handle = bank.request_line(line, owner)?;
            bank.set_direction(&handle, Direction::Output { initial: value })?;
            bank.write(&handle, value)?;
            bank.release_line(handle)?;
            return Ok(());
        }

        let pin = Self::pin_from_gpio_ranges(interface, line)?;
        interface.apply_config(&ConfigSetting::pin(pin, PinConfig::OutputValue(value)))?;
        interface.apply_config(&ConfigSetting::pin(pin, PinConfig::OutputEnable(true)))
    }

    fn pin_from_gpio_ranges(
        interface: &(impl Interface + ?Sized),
        line: GpioLineId,
    ) -> Result<PinId, PinctrlError> {
        for range in interface.gpio_ranges() {
            let end = range.line_base.saturating_add(range.count);
            if range.bank == line.bank && (range.line_base..end).contains(&line.offset) {
                return Ok(PinId::new(range.pin_base + line.offset - range.line_base));
            }
        }
        Err(PinctrlError::InvalidLine(line))
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, vec, vec::Vec};

    use fdt_edit::{Fdt, Node, NodeType, Phandle, Property};

    use crate::{
        ConfigSetting, Direction, DriverGeneric, FdtPinctrl, FdtPinctrlParser, FunctionId,
        GpioBank, GpioBankId, GpioLineHandle, GpioLineId, GpioRange, GroupId, Interface,
        MuxSetting, MuxValue, PinConfig, PinDesc, PinFunction, PinGroup, PinId, PinState,
        PinctrlDevice, PinctrlError, StateName,
    };

    struct TestParser;

    impl FdtPinctrlParser for TestParser {
        fn parse_pinctrl_node(
            &self,
            _fdt: &Fdt,
            node: NodeType<'_>,
            state: &mut PinState,
        ) -> Result<(), PinctrlError> {
            let cells = node
                .as_node()
                .get_property("test,pins")
                .ok_or(PinctrlError::InvalidConfig)?
                .get_u32_iter()
                .collect::<Vec<_>>();
            for pin in cells.chunks_exact(3) {
                let raw_pin = pin[0];
                let function = pin[1];
                let mux = pin[2];
                state.push_mux(MuxSetting::new(
                    GroupId::new(raw_pin),
                    FunctionId::new(function),
                    MuxValue::new(mux),
                ));
                state.push_config(ConfigSetting::pin(
                    PinId::new(raw_pin),
                    PinConfig::DriveStrengthUa(8000),
                ));
            }
            Ok(())
        }

        fn parse_gpio_line(
            &self,
            fdt: &Fdt,
            consumer: &Node,
            prop_name: &str,
        ) -> Option<Result<GpioLineId, PinctrlError>> {
            let mut cells = consumer.get_property(prop_name)?.get_u32_iter();
            let phandle = Phandle::from(cells.next()?);
            let offset = cells.next()?;
            let gpio = fdt.get_by_phandle(phandle)?;
            let bank = gpio
                .as_node()
                .name()
                .strip_prefix("gpio")
                .and_then(|name| name.chars().next())
                .and_then(|ch| ch.to_digit(10))?;
            Some(Ok(GpioLineId::new(GpioBankId::new(bank), offset)))
        }

        fn gpio_lines_from_state(&self, state: &PinState) -> Result<Vec<GpioLineId>, PinctrlError> {
            Ok(state
                .muxes()
                .iter()
                .map(|mux| {
                    GpioLineId::new(GpioBankId::new(mux.group.raw() / 32), mux.group.raw() % 32)
                })
                .collect())
        }
    }

    struct Recorder {
        calls: Vec<&'static str>,
        configs: Vec<ConfigSetting>,
        pins: Vec<PinDesc>,
        groups: Vec<PinGroup>,
        functions: Vec<PinFunction>,
        gpio_ranges: Vec<GpioRange>,
        use_gpio_bank: bool,
    }

    impl Recorder {
        fn new() -> Self {
            let pins = (0..96)
                .map(|pin| PinDesc::new(PinId::new(pin), None))
                .collect();
            let groups = (0..96)
                .map(|pin| PinGroup::new(GroupId::new(pin), None, vec![PinId::new(pin)]))
                .collect();
            let functions = (0..16)
                .map(|function| {
                    PinFunction::new(
                        FunctionId::new(function),
                        None,
                        (0..96).map(GroupId::new).collect(),
                    )
                })
                .collect();
            Self {
                calls: Vec::new(),
                configs: Vec::new(),
                pins,
                groups,
                functions,
                gpio_ranges: vec![
                    GpioRange::new(GpioBankId::new(0), 0, 0, 32),
                    GpioRange::new(GpioBankId::new(1), 32, 0, 32),
                    GpioRange::new(GpioBankId::new(2), 64, 0, 32),
                ],
                use_gpio_bank: false,
            }
        }

        fn with_gpio_bank(mut self) -> Self {
            self.use_gpio_bank = true;
            self
        }
    }

    impl DriverGeneric for Recorder {
        fn name(&self) -> &str {
            "recorder"
        }
    }

    impl Interface for Recorder {
        fn pins(&self) -> &[PinDesc] {
            &self.pins
        }

        fn groups(&self) -> &[PinGroup] {
            &self.groups
        }

        fn functions(&self) -> &[PinFunction] {
            &self.functions
        }

        fn gpio_ranges(&self) -> &[GpioRange] {
            &self.gpio_ranges
        }

        fn apply_mux(&mut self, _setting: &MuxSetting) -> Result<(), PinctrlError> {
            self.calls.push("mux");
            Ok(())
        }

        fn apply_config(&mut self, setting: &ConfigSetting) -> Result<(), PinctrlError> {
            self.calls.push("config");
            self.configs.push(*setting);
            Ok(())
        }

        fn create_gpio_bank(&mut self, bank_id: GpioBankId) -> Option<Box<dyn GpioBank>> {
            self.use_gpio_bank
                .then(|| Box::new(RecordingBank::new(bank_id)) as Box<dyn GpioBank>)
        }
    }

    struct RecordingBank {
        bank_id: GpioBankId,
        requested: Option<GpioLineHandle>,
        value: bool,
    }

    impl RecordingBank {
        fn new(bank_id: GpioBankId) -> Self {
            Self {
                bank_id,
                requested: None,
                value: false,
            }
        }

        fn ensure_requested(&self, handle: &GpioLineHandle) -> Result<(), PinctrlError> {
            if self.requested.as_ref() == Some(handle) {
                Ok(())
            } else {
                Err(PinctrlError::LineNotRequested(handle.line()))
            }
        }
    }

    impl GpioBank for RecordingBank {
        fn bank_id(&self) -> GpioBankId {
            self.bank_id
        }

        fn line_count(&self) -> u32 {
            32
        }

        fn request_line(
            &mut self,
            line: GpioLineId,
            _owner: &str,
        ) -> Result<GpioLineHandle, PinctrlError> {
            let handle = GpioLineHandle::new(line, crate::OwnerId::new(1));
            self.requested = Some(handle);
            Ok(handle)
        }

        fn release_line(&mut self, handle: GpioLineHandle) -> Result<(), PinctrlError> {
            self.ensure_requested(&handle)?;
            self.requested = None;
            Ok(())
        }

        fn set_direction(
            &mut self,
            handle: &GpioLineHandle,
            direction: Direction,
        ) -> Result<(), PinctrlError> {
            self.ensure_requested(handle)?;
            match direction {
                Direction::Input => {}
                Direction::Output { initial } => self.value = initial,
            }
            Ok(())
        }

        fn read(&self, handle: &GpioLineHandle) -> Result<bool, PinctrlError> {
            self.ensure_requested(handle)?;
            Ok(self.value)
        }

        fn write(&mut self, handle: &GpioLineHandle, value: bool) -> Result<(), PinctrlError> {
            self.ensure_requested(handle)?;
            self.value = value;
            Ok(())
        }
    }

    #[test]
    fn pinctrl_names_and_pinctrl_0_create_default_state() {
        let (fdt, consumer) = fdt_with_consumer(&["default"], &[10]);

        let state =
            FdtPinctrl::state_from_consumer(&fdt, fdt.node(consumer).unwrap(), 0, &TestParser)
                .unwrap();

        assert_eq!(state.name(), &StateName::Default);
        assert_eq!(state.muxes()[0].group, GroupId::new(34));
        assert_eq!(state.muxes()[0].value.raw(), 7);
    }

    #[test]
    fn multiple_pinctrl_phandles_merge_into_one_state() {
        let (fdt, consumer) = fdt_with_consumer(&["default"], &[10, 11]);

        let state =
            FdtPinctrl::state_from_consumer(&fdt, fdt.node(consumer).unwrap(), 0, &TestParser)
                .unwrap();

        assert_eq!(state.muxes().len(), 2);
        assert_eq!(state.muxes()[0].group, GroupId::new(34));
        assert_eq!(state.muxes()[1].group, GroupId::new(65));
    }

    #[test]
    fn apply_state_from_consumer_keeps_mux_before_config() {
        let (fdt, consumer) = fdt_with_consumer(&["default"], &[10]);
        let mut recorder = Recorder::new();

        FdtPinctrl::apply_state_from_consumer(
            &mut recorder,
            &fdt,
            fdt.node(consumer).unwrap(),
            0,
            &TestParser,
        )
        .unwrap();

        assert_eq!(recorder.calls, vec!["mux", "config"]);
    }

    #[test]
    fn pinctrl_device_applies_fdt_default_state_with_registered_parser() {
        let (fdt, consumer) = fdt_with_consumer(&["default"], &[10]);
        let mut device = PinctrlDevice::with_fdt_parser(Recorder::new(), TestParser);

        device
            .apply_fdt_default_state(&fdt, fdt.node(consumer).unwrap())
            .unwrap();

        assert_eq!(
            device.typed_ref::<Recorder>().unwrap().calls,
            vec!["mux", "config"]
        );
    }

    #[test]
    fn pinctrl_device_skips_fdt_default_state_without_property_or_parser() {
        let (fdt, consumer) = fdt_with_consumer(&["default"], &[10]);
        let mut without_parser = PinctrlDevice::new(Recorder::new());
        without_parser
            .apply_fdt_default_state(&fdt, fdt.node(consumer).unwrap())
            .unwrap();
        assert!(
            without_parser
                .typed_ref::<Recorder>()
                .unwrap()
                .calls
                .is_empty()
        );

        let mut fdt_without_pinctrl = Fdt::new();
        let root = fdt_without_pinctrl.root_id();
        let consumer_without_pinctrl = fdt_without_pinctrl.add_node(root, Node::new("consumer"));
        let mut with_parser = PinctrlDevice::with_fdt_parser(Recorder::new(), TestParser);
        with_parser
            .apply_fdt_default_state(
                &fdt_without_pinctrl,
                fdt_without_pinctrl.node(consumer_without_pinctrl).unwrap(),
            )
            .unwrap();
        assert!(
            with_parser
                .typed_ref::<Recorder>()
                .unwrap()
                .calls
                .is_empty()
        );
    }

    #[test]
    fn fixed_regulator_uses_gpio_flags_and_enable_active_low_for_output_value() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            node_with_props("gpio1@0", &[prop_u32s("phandle", &[20])]),
        );
        let regulator = fdt.add_node(
            root,
            node_with_props(
                "vbus-regulator",
                &[
                    prop_u32s("gpios", &[20, 5, 1]),
                    Property::new("enable-active-low", Vec::new()),
                ],
            ),
        );
        let mut recorder = Recorder::new();

        FdtPinctrl::apply_fixed_regulator(
            &mut recorder,
            &fdt,
            fdt.node(regulator).unwrap(),
            &TestParser,
            "dwc-xhci-vbus",
        )
        .unwrap();

        assert!(recorder.configs.contains(&ConfigSetting::pin(
            PinId::new(37),
            PinConfig::OutputEnable(true)
        )));
        assert!(recorder.configs.contains(&ConfigSetting::pin(
            PinId::new(37),
            PinConfig::OutputValue(true)
        )));
    }

    #[test]
    fn fixed_regulator_sets_output_latch_before_enabling_output() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            node_with_props("gpio1@0", &[prop_u32s("phandle", &[20])]),
        );
        let regulator = fdt.add_node(
            root,
            node_with_props("vbus-regulator", &[prop_u32s("gpios", &[20, 5, 0])]),
        );
        let mut recorder = Recorder::new();

        FdtPinctrl::apply_fixed_regulator(
            &mut recorder,
            &fdt,
            fdt.node(regulator).unwrap(),
            &TestParser,
            "dwc-xhci-vbus",
        )
        .unwrap();

        let pin = PinId::new(37);
        let configs = recorder
            .configs
            .iter()
            .filter(|setting| setting.target == crate::ConfigTarget::Pin(pin))
            .map(|setting| setting.config)
            .collect::<Vec<_>>();

        assert_eq!(
            configs,
            vec![PinConfig::OutputValue(true), PinConfig::OutputEnable(true)]
        );
    }

    #[test]
    fn fixed_regulator_applies_pinctrl_0_before_driving_gpio() {
        let (mut fdt, _consumer) = fdt_with_consumer(&["default"], &[10]);
        let root = fdt.root_id();
        let regulator = fdt.add_node(
            root,
            node_with_props(
                "vbus-regulator",
                &[
                    prop_strs("pinctrl-names", &["default"]),
                    prop_u32s("pinctrl-0", &[10]),
                ],
            ),
        );
        let mut recorder = Recorder::new();

        FdtPinctrl::apply_fixed_regulator(
            &mut recorder,
            &fdt,
            fdt.node(regulator).unwrap(),
            &TestParser,
            "dwc-xhci-vbus",
        )
        .unwrap();

        assert_eq!(recorder.calls, vec!["mux", "config", "config", "config"]);
        assert!(recorder.configs.contains(&ConfigSetting::pin(
            PinId::new(34),
            PinConfig::OutputValue(true)
        )));
    }

    #[test]
    fn unsupported_firmware_reports_explicit_error() {
        assert_eq!(
            FdtPinctrl::unsupported_firmware(crate::FirmwareKind::Acpi),
            PinctrlError::UnsupportedFirmware(crate::FirmwareKind::Acpi)
        );
    }

    #[test]
    fn fixed_regulator_can_use_gpio_bank_endpoint() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            node_with_props("gpio1@0", &[prop_u32s("phandle", &[20])]),
        );
        let regulator = fdt.add_node(
            root,
            node_with_props("vbus-regulator", &[prop_u32s("gpios", &[20, 5, 0])]),
        );
        let mut recorder = Recorder::new().with_gpio_bank();

        FdtPinctrl::apply_fixed_regulator(
            &mut recorder,
            &fdt,
            fdt.node(regulator).unwrap(),
            &TestParser,
            "dwc-xhci-vbus",
        )
        .unwrap();

        assert!(recorder.configs.is_empty());
    }

    fn fdt_with_consumer(names: &[&str], phandles: &[u32]) -> (Fdt, fdt_edit::NodeId) {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            node_with_props(
                "pin-state-a",
                &[
                    prop_u32s("phandle", &[10]),
                    prop_u32s("test,pins", &[34, 2, 7]),
                ],
            ),
        );
        fdt.add_node(
            root,
            node_with_props(
                "pin-state-b",
                &[
                    prop_u32s("phandle", &[11]),
                    prop_u32s("test,pins", &[65, 3, 8]),
                ],
            ),
        );
        let consumer = fdt.add_node(
            root,
            node_with_props(
                "consumer",
                &[
                    prop_strs("pinctrl-names", names),
                    prop_u32s("pinctrl-0", phandles),
                ],
            ),
        );
        (fdt, consumer)
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
