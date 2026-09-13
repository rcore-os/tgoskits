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
    use alloc::{vec, vec::Vec};

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
}
