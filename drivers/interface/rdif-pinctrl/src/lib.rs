#![no_std]

extern crate alloc;

mod error;
#[cfg(feature = "fdt")]
mod fdt;
mod gpio;
mod id;
mod interface;
mod irq;
mod types;

pub use error::*;
#[cfg(feature = "fdt")]
pub use fdt::*;
pub use gpio::*;
pub use id::*;
pub use interface::*;
pub use irq::*;
pub use rdif_base::{DriverGeneric, KError, io};
pub use types::*;

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, string::String, vec, vec::Vec};

    use super::*;

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

        fn apply_mux(&mut self, _setting: &MuxSetting) -> Result<(), PinctrlError> {
            self.calls.push("mux");
            Ok(())
        }

        fn apply_config(&mut self, _setting: &ConfigSetting) -> Result<(), PinctrlError> {
            self.calls.push("config");
            Ok(())
        }
    }

    #[test]
    fn apply_state_programs_mux_before_config() {
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

        recorder.apply_state(&state).unwrap();

        assert_eq!(recorder.calls, vec!["mux", "config"]);
    }

    #[test]
    fn validate_state_rejects_invalid_group_function_pair() {
        let recorder = Recorder::new();
        let state = PinState::named(StateName::Default).with_mux(MuxSetting::new(
            GroupId::new(1),
            FunctionId::new(99),
            MuxValue::new(2),
        ));

        assert_eq!(
            recorder.validate_state(&state),
            Err(PinctrlError::InvalidFunction(FunctionId::new(99)))
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
            assert_eq!(owner, "uart");
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
            _direction: Direction,
        ) -> Result<(), PinctrlError> {
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

    #[test]
    fn gpio_line_handle_is_required_for_line_io() {
        let mut bank = MockBank::new();
        let line = GpioLineId::new(GpioBankId::new(0), 3);
        let forged = GpioLineHandle::new(line, OwnerId::new(9));

        assert_eq!(
            bank.write(&forged, true),
            Err(PinctrlError::LineNotRequested(line))
        );

        let handle = bank.request_line(line, "uart").unwrap();
        bank.set_direction(&handle, Direction::Output { initial: false })
            .unwrap();
        bank.write(&handle, true).unwrap();
        assert!(bank.read(&handle).unwrap());
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

    #[test]
    fn gpio_irq_handler_reports_stable_event_without_os_types() {
        let mut irq = MockIrq {
            line: GpioLineId::new(GpioBankId::new(2), 5),
        };

        let event = irq.handle_irq();

        assert_eq!(event.lines().len(), 1);
        assert_eq!(
            event.lines()[0].line,
            GpioLineId::new(GpioBankId::new(2), 5)
        );
        assert_eq!(event.lines()[0].trigger, GpioIrqTrigger::EdgeRising);
    }

    #[test]
    fn acpi_firmware_specs_can_represent_explicit_unsupported_mapping() {
        let spec = AcpiPinStateSpec::new(String::from("\\_SB.GPIO"), StateName::Default);

        assert_eq!(spec.state_name(), &StateName::Default);
        assert_eq!(
            PinctrlError::UnsupportedFirmware(FirmwareKind::Acpi),
            PinctrlError::UnsupportedFirmware(FirmwareKind::Acpi)
        );
    }

    #[test]
    fn interface_can_transfer_gpio_irq_handler_ownership() {
        struct Device {
            handler: Option<Box<dyn GpioIrqHandler>>,
        }

        impl DriverGeneric for Device {
            fn name(&self) -> &str {
                "dev"
            }
        }

        impl Interface for Device {
            fn take_irq_handler(
                &mut self,
                source_id: GpioIrqSourceId,
            ) -> Option<Box<dyn GpioIrqHandler>> {
                if source_id == GpioIrqSourceId::new(0) {
                    self.handler.take()
                } else {
                    None
                }
            }
        }

        let mut device = Device {
            handler: Some(Box::new(MockIrq {
                line: GpioLineId::new(GpioBankId::new(0), 1),
            })),
        };

        assert!(device.take_irq_handler(GpioIrqSourceId::new(0)).is_some());
        assert!(device.take_irq_handler(GpioIrqSourceId::new(0)).is_none());
    }

    #[test]
    fn pinctrl_device_wraps_interface_as_queryable_capability() {
        let mut device = PinctrlDevice::new(Recorder::new());
        let state = PinState::named(StateName::Default).with_mux(MuxSetting::new(
            GroupId::new(1),
            FunctionId::new(1),
            MuxValue::new(8),
        ));

        device.apply_state(&state).unwrap();

        assert_eq!(device.name(), "recorder");
        assert!(device.typed_ref::<Recorder>().is_some());
        assert_eq!(device.typed_mut::<Recorder>().unwrap().calls, vec!["mux"]);
    }
}
