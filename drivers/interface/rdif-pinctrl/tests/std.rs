extern crate alloc;

use alloc::{vec, vec::Vec};

use rdif_pinctrl::{
    ConfigSetting, FirmwareKind, FunctionId, GpioBankId, GpioIrqError, GpioIrqEvent,
    GpioIrqSourceId, GpioIrqSourceInfo, GpioIrqTrigger, GpioLineEvent, GpioLineId, GroupId,
    Interface, MuxSetting, MuxValue, PinConfig, PinDesc, PinFunction, PinGroup, PinId, PinState,
    PinctrlError, StateName, io,
};

struct Recorder {
    calls: Vec<&'static str>,
    pins: Vec<PinDesc>,
    groups: Vec<PinGroup>,
    functions: Vec<PinFunction>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            calls: Vec::new(),
            pins: vec![PinDesc::new(PinId::new(1), Some("gpio1"))],
            groups: vec![PinGroup::new(
                GroupId::new(1),
                Some("uart0"),
                vec![PinId::new(1)],
            )],
            functions: vec![PinFunction::new(
                FunctionId::new(1),
                Some("uart"),
                vec![GroupId::new(1)],
            )],
        }
    }
}

impl rdif_pinctrl::DriverGeneric for Recorder {
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

    fn apply_mux(&mut self, setting: &MuxSetting) -> Result<(), PinctrlError> {
        if setting.value.raw() != 8 {
            return Err(PinctrlError::InvalidConfig);
        }
        self.calls.push("mux");
        Ok(())
    }

    fn apply_config(&mut self, setting: &ConfigSetting) -> Result<(), PinctrlError> {
        if !matches!(setting.config, PinConfig::DriveStrengthUa(8000)) {
            return Err(PinctrlError::InvalidConfig);
        }
        self.calls.push("config");
        Ok(())
    }

    fn irq_sources(&self) -> Vec<GpioIrqSourceInfo> {
        vec![GpioIrqSourceInfo::new(
            GpioIrqSourceId::new(1),
            vec![GpioLineId::new(GpioBankId::new(0), 3)],
        )]
    }
}

#[test]
fn rdif_pinctrl_interface_validates_and_applies_state_in_order() {
    let mut recorder = Recorder::new();
    let state = PinState::named(StateName::Default)
        .with_mux(MuxSetting::new(
            GroupId::new(1),
            FunctionId::new(1),
            MuxValue::new(8),
        ))
        .with_config(ConfigSetting::pin(
            PinId::new(1),
            PinConfig::DriveStrengthUa(8000),
        ));

    recorder.validate_state(&state).unwrap();
    recorder.apply_state(&state).unwrap();
    assert_eq!(recorder.calls, vec!["mux", "config"]);
    assert!(recorder.can_mux(GroupId::new(1), FunctionId::new(1)));
    assert!(!recorder.can_mux(GroupId::new(9), FunctionId::new(1)));
}

#[test]
fn rdif_pinctrl_validation_reports_specific_invalid_state_parts() {
    let recorder = Recorder::new();

    let missing_group = PinState::named(StateName::Default).with_mux(MuxSetting::new(
        GroupId::new(99),
        FunctionId::new(1),
        MuxValue::new(2),
    ));
    assert_eq!(
        recorder.validate_state(&missing_group),
        Err(PinctrlError::InvalidGroup(GroupId::new(99)))
    );

    let missing_function = PinState::named(StateName::Default).with_mux(MuxSetting::new(
        GroupId::new(1),
        FunctionId::new(99),
        MuxValue::new(2),
    ));
    assert_eq!(
        recorder.validate_state(&missing_function),
        Err(PinctrlError::InvalidFunction(FunctionId::new(99)))
    );

    let missing_pin = PinState::named(StateName::Default).with_config(ConfigSetting::pin(
        PinId::new(99),
        PinConfig::InputEnable(true),
    ));
    assert_eq!(
        recorder.validate_state(&missing_pin),
        Err(PinctrlError::InvalidPin(PinId::new(99)))
    );
}

#[test]
fn rdif_pinctrl_gpio_irq_event_reports_line_overflow() {
    let mut event = GpioIrqEvent::none();

    for offset in 0..rdif_pinctrl::MAX_GPIO_IRQ_EVENTS {
        assert!(event.push_line(GpioLineEvent::new(
            GpioLineId::new(GpioBankId::new(0), offset as u32),
            GpioIrqTrigger::EdgeBoth,
        )));
    }
    assert!(!event.push_line(GpioLineEvent::new(
        GpioLineId::new(GpioBankId::new(0), 99),
        GpioIrqTrigger::LevelLow,
    )));
    assert_eq!(event.lines().len(), rdif_pinctrl::MAX_GPIO_IRQ_EVENTS);
    assert_eq!(event.error(), Some(GpioIrqError::Overflow));
}

#[test]
fn rdif_pinctrl_errors_map_to_io_kinds() {
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::NotSupported),
        io::ErrorKind::Unsupported
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::UnsupportedFirmware(FirmwareKind::Acpi)),
        io::ErrorKind::Unsupported
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::NotAvailable),
        io::ErrorKind::NotAvailable
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidPin(PinId::new(1))),
        io::ErrorKind::InvalidData
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidGroup(GroupId::new(1))),
        io::ErrorKind::InvalidData
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidFunction(FunctionId::new(1))),
        io::ErrorKind::InvalidData
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidMux {
            group: GroupId::new(1),
            function: FunctionId::new(2)
        }),
        io::ErrorKind::InvalidData
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidLine(GpioLineId::new(
            GpioBankId::new(0),
            1
        ))),
        io::ErrorKind::InvalidData
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidConfig),
        io::ErrorKind::InvalidData
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::LineBusy(GpioLineId::new(
            GpioBankId::new(0),
            1
        ))),
        io::ErrorKind::Interrupted
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::LineNotRequested(GpioLineId::new(
            GpioBankId::new(0),
            1
        ))),
        io::ErrorKind::Other(_)
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::IrqEventOverflow),
        io::ErrorKind::Other(_)
    ));
    assert!(matches!(
        io::ErrorKind::from(PinctrlError::other("opaque")),
        io::ErrorKind::Other(_)
    ));
}
