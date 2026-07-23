use alloc::{boxed::Box, string::String, vec, vec::Vec};

use axtest::prelude::*;
use rdif_base::DriverGeneric;

use crate::{
    AcpiGpioLineSpec, AcpiPinStateSpec, Bias, ConfigSetting, ConfigTarget, Direction, FirmwareKind,
    FunctionId, GpioBank, GpioBankId, GpioIrqError, GpioIrqEvent, GpioIrqHandler, GpioIrqSourceId,
    GpioIrqSourceInfo, GpioIrqTrigger, GpioLineEvent, GpioLineHandle, GpioLineId, GpioRange,
    GroupId, Interface, LowPowerMode, MuxSetting, MuxValue, OwnerId, PinConfig, PinDesc,
    PinFunction, PinGroup, PinId, PinState, PinctrlDevice, PinctrlError, SlewRate, StateName, io,
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

impl crate::DriverGeneric for Recorder {
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

#[axtest]
fn rdif_pinctrl_ids_and_plain_data_keep_raw_values() {
    ax_assert_eq!(PinId::new(1).raw(), 1);
    ax_assert_eq!(GroupId::new(2).raw(), 2);
    ax_assert_eq!(FunctionId::new(3).raw(), 3);
    ax_assert_eq!(GpioBankId::new(4).raw(), 4);
    ax_assert_eq!(GpioIrqSourceId::new(5).raw(), 5);
    ax_assert_eq!(OwnerId::new(6).raw(), 6);

    let range = GpioRange::new(GpioBankId::new(1), 10, 20, 4);
    ax_assert_eq!(range.pin_base, 10);
    ax_assert_eq!(range.line_base, 20);
    ax_assert_eq!(range.count, 4);

    let spec = AcpiGpioLineSpec::new(
        String::from("\\_SB.GPIO"),
        GpioLineId::new(GpioBankId::new(0), 7),
        2,
    );
    ax_assert_eq!(spec.path(), "\\_SB.GPIO");
    ax_assert_eq!(spec.line().offset, 7);
    ax_assert_eq!(spec.resource_index(), 2);
}

#[axtest]
fn rdif_pinctrl_pin_state_builders_accumulate_muxes_and_configs() {
    let mut state = PinState::named(StateName::Named(String::from("uart")));
    state.push_mux(MuxSetting::new(
        GroupId::new(1),
        FunctionId::new(1),
        MuxValue::new(8),
    ));
    state.push_config(ConfigSetting::pin(
        PinId::new(1),
        PinConfig::Bias(Bias::PullUp),
    ));
    state.extend(
        PinState::named(StateName::Sleep).with_config(ConfigSetting::group(
            GroupId::new(1),
            PinConfig::LowPowerMode(LowPowerMode::HiZ),
        )),
    );

    ax_assert!(matches!(state.name(), StateName::Named(_)));
    ax_assert_eq!(state.muxes().len(), 1);
    ax_assert_eq!(state.configs().len(), 2);
    ax_assert!(matches!(
        state.configs()[0].target,
        ConfigTarget::Pin(pin) if pin == PinId::new(1)
    ));
    ax_assert!(matches!(
        PinConfig::SlewRate(SlewRate::Raw(3)),
        PinConfig::SlewRate(SlewRate::Raw(3))
    ));
    ax_assert!(matches!(
        PinConfig::Vendor { param: 1, value: 2 },
        PinConfig::Vendor { param: 1, value: 2 }
    ));
}

#[axtest]
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
    ax_assert_eq!(recorder.calls, vec!["mux", "config"]);
    ax_assert!(recorder.can_mux(GroupId::new(1), FunctionId::new(1)));
    ax_assert!(!recorder.can_mux(GroupId::new(9), FunctionId::new(1)));
}

#[axtest]
fn rdif_pinctrl_validation_reports_specific_invalid_state_parts() {
    let recorder = Recorder::new();

    let missing_group = PinState::named(StateName::Default).with_mux(MuxSetting::new(
        GroupId::new(99),
        FunctionId::new(1),
        MuxValue::new(2),
    ));
    ax_assert_eq!(
        recorder.validate_state(&missing_group),
        Err(PinctrlError::InvalidGroup(GroupId::new(99)))
    );

    let missing_function = PinState::named(StateName::Default).with_mux(MuxSetting::new(
        GroupId::new(1),
        FunctionId::new(99),
        MuxValue::new(2),
    ));
    ax_assert_eq!(
        recorder.validate_state(&missing_function),
        Err(PinctrlError::InvalidFunction(FunctionId::new(99)))
    );

    let missing_pin = PinState::named(StateName::Default).with_config(ConfigSetting::pin(
        PinId::new(99),
        PinConfig::InputEnable(true),
    ));
    ax_assert_eq!(
        recorder.validate_state(&missing_pin),
        Err(PinctrlError::InvalidPin(PinId::new(99)))
    );
}

struct MockBank {
    requested: Option<GpioLineHandle>,
    value: bool,
}

impl MockBank {
    fn new() -> Self {
        Self {
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

impl GpioBank for MockBank {
    fn bank_id(&self) -> GpioBankId {
        GpioBankId::new(0)
    }

    fn line_count(&self) -> u32 {
        8
    }

    fn request_line(
        &mut self,
        line: GpioLineId,
        owner: &str,
    ) -> Result<GpioLineHandle, PinctrlError> {
        if owner != "uart" {
            return Err(PinctrlError::InvalidConfig);
        }
        let handle = GpioLineHandle::new(line, OwnerId::new(7));
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
        if !matches!(direction, Direction::Output { initial: false }) {
            return Err(PinctrlError::InvalidConfig);
        }
        self.ensure_requested(handle)
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

#[axtest]
fn rdif_pinctrl_gpio_line_handle_authorizes_bank_access() {
    let mut bank = MockBank::new();
    let line = GpioLineId::new(GpioBankId::new(0), 3);
    let forged = GpioLineHandle::new(line, OwnerId::new(9));

    ax_assert_eq!(bank.bank_id(), GpioBankId::new(0));
    ax_assert_eq!(bank.line_count(), 8);
    ax_assert_eq!(
        bank.write(&forged, true),
        Err(PinctrlError::LineNotRequested(line))
    );

    let handle = bank.request_line(line, "uart").unwrap();
    ax_assert_eq!(handle.line(), line);
    ax_assert_eq!(handle.owner(), OwnerId::new(7));
    bank.set_direction(&handle, Direction::Output { initial: false })
        .unwrap();
    bank.write(&handle, true).unwrap();
    ax_assert!(bank.read(&handle).unwrap());
    bank.release_line(handle).unwrap();
}

struct MockIrq {
    line: GpioLineId,
}

impl GpioIrqHandler for MockIrq {
    fn handle_irq(&mut self) -> GpioIrqEvent {
        GpioIrqEvent::from_line(GpioLineEvent::new(self.line, GpioIrqTrigger::EdgeRising))
    }
}

#[axtest]
fn rdif_pinctrl_gpio_irq_event_tracks_sources_lines_and_overflow() {
    let source = GpioIrqSourceId::new(3);
    let mut event = GpioIrqEvent::none();
    ax_assert!(event.is_empty());
    event.set_source(source);
    ax_assert_eq!(event.source(), Some(source));

    for offset in 0..crate::MAX_GPIO_IRQ_EVENTS {
        ax_assert!(event.push_line(GpioLineEvent::new(
            GpioLineId::new(GpioBankId::new(0), offset as u32),
            GpioIrqTrigger::EdgeBoth,
        )));
    }
    ax_assert!(!event.push_line(GpioLineEvent::new(
        GpioLineId::new(GpioBankId::new(0), 99),
        GpioIrqTrigger::LevelLow,
    )));
    ax_assert_eq!(event.lines().len(), crate::MAX_GPIO_IRQ_EVENTS);
    ax_assert_eq!(event.error(), Some(GpioIrqError::Overflow));

    let mut handler = MockIrq {
        line: GpioLineId::new(GpioBankId::new(2), 5),
    };
    let handled = handler.handle_irq();
    ax_assert_eq!(handled.lines()[0].trigger, GpioIrqTrigger::EdgeRising);
    ax_assert_eq!(
        GpioIrqEvent::with_error(GpioIrqError::Spurious).error(),
        Some(GpioIrqError::Spurious)
    );
}

#[axtest]
fn rdif_pinctrl_device_wrapper_delegates_interface_and_downcast() {
    let recorder = Recorder::new();
    let mut device = PinctrlDevice::new(recorder);

    ax_assert_eq!(device.name(), "recorder");
    ax_assert_eq!(device.pins().len(), 1);
    ax_assert_eq!(device.interface().groups().len(), 1);
    ax_assert!(device.typed_ref::<Recorder>().is_some());
    ax_assert!(device.typed_mut::<Recorder>().is_some());
    ax_assert_eq!(device.irq_sources()[0].id.raw(), 1);

    let boxed: Box<dyn Interface> = Box::new(Recorder::new());
    let device = PinctrlDevice::boxed(boxed);
    ax_assert_eq!(device.name(), "recorder");
}

#[axtest]
fn rdif_pinctrl_errors_compare_and_map_to_io_kinds() {
    let pin_error = PinctrlError::InvalidPin(PinId::new(1));
    ax_assert_eq!(pin_error, PinctrlError::InvalidPin(PinId::new(1)));
    ax_assert_ne!(pin_error, PinctrlError::InvalidPin(PinId::new(2)));
    ax_assert_eq!(PinctrlError::NotSupported, PinctrlError::NotSupported);
    ax_assert_eq!(
        PinctrlError::UnsupportedFirmware(FirmwareKind::Fdt),
        PinctrlError::UnsupportedFirmware(FirmwareKind::Fdt)
    );
    ax_assert_ne!(
        PinctrlError::UnsupportedFirmware(FirmwareKind::Fdt),
        PinctrlError::UnsupportedFirmware(FirmwareKind::Acpi)
    );
    ax_assert_eq!(
        PinctrlError::InvalidGroup(GroupId::new(1)),
        PinctrlError::InvalidGroup(GroupId::new(1))
    );
    ax_assert_eq!(
        PinctrlError::InvalidFunction(FunctionId::new(1)),
        PinctrlError::InvalidFunction(FunctionId::new(1))
    );
    ax_assert_eq!(
        PinctrlError::InvalidMux {
            group: GroupId::new(1),
            function: FunctionId::new(2),
        },
        PinctrlError::InvalidMux {
            group: GroupId::new(1),
            function: FunctionId::new(2),
        }
    );
    ax_assert_eq!(
        PinctrlError::InvalidLine(GpioLineId::new(GpioBankId::new(0), 1)),
        PinctrlError::InvalidLine(GpioLineId::new(GpioBankId::new(0), 1))
    );
    ax_assert_eq!(
        PinctrlError::LineBusy(GpioLineId::new(GpioBankId::new(0), 1)),
        PinctrlError::LineBusy(GpioLineId::new(GpioBankId::new(0), 1))
    );
    ax_assert_eq!(
        PinctrlError::LineNotRequested(GpioLineId::new(GpioBankId::new(0), 1)),
        PinctrlError::LineNotRequested(GpioLineId::new(GpioBankId::new(0), 1))
    );
    ax_assert_eq!(PinctrlError::InvalidConfig, PinctrlError::InvalidConfig);
    ax_assert_eq!(
        PinctrlError::IrqEventOverflow,
        PinctrlError::IrqEventOverflow
    );
    ax_assert_ne!(PinctrlError::other("left"), PinctrlError::other("right"));

    let acpi = AcpiPinStateSpec::new(String::from("\\_SB.PIN"), StateName::Default);
    ax_assert_eq!(acpi.path(), "\\_SB.PIN");
    ax_assert_eq!(acpi.state_name(), &StateName::Default);

    ax_assert_eq!(
        alloc::format!("{}", PinctrlError::InvalidPin(PinId::new(7))),
        "invalid pin: PinId(7)"
    );
    ax_assert_eq!(
        alloc::format!("{}", PinctrlError::other("opaque")),
        "other error: opaque"
    );

    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::NotSupported),
        io::ErrorKind::Unsupported
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::UnsupportedFirmware(FirmwareKind::Acpi)),
        io::ErrorKind::Unsupported
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::NotAvailable),
        io::ErrorKind::NotAvailable
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidPin(PinId::new(1))),
        io::ErrorKind::InvalidData
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidGroup(GroupId::new(1))),
        io::ErrorKind::InvalidData
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidFunction(FunctionId::new(1))),
        io::ErrorKind::InvalidData
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidMux {
            group: GroupId::new(1),
            function: FunctionId::new(2)
        }),
        io::ErrorKind::InvalidData
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidLine(GpioLineId::new(
            GpioBankId::new(0),
            1
        ))),
        io::ErrorKind::InvalidData
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::InvalidConfig),
        io::ErrorKind::InvalidData
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::LineBusy(GpioLineId::new(
            GpioBankId::new(0),
            1
        ))),
        io::ErrorKind::Interrupted
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::LineNotRequested(GpioLineId::new(
            GpioBankId::new(0),
            1
        ))),
        io::ErrorKind::Other(_)
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::IrqEventOverflow),
        io::ErrorKind::Other(_)
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(PinctrlError::other("opaque")),
        io::ErrorKind::Other(_)
    ));
}
