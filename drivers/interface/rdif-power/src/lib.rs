#![no_std]

extern crate alloc;

use rdif_base::def_driver;
pub use rdif_base::{DriverGeneric, custom_type};

custom_type!(
    #[doc = "Power domain id"],
    PowerDomainId, u64, "{:#x}");

impl PowerDomainId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

impl From<u32> for PowerDomainId {
    fn from(value: u32) -> Self {
        Self(u64::from(value))
    }
}

impl From<usize> for PowerDomainId {
    fn from(value: usize) -> Self {
        Self(value as u64)
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerError {
    #[error("invalid power domain id")]
    InvalidId,
    #[error("unsupported power operation")]
    Unsupported,
    #[error("power controller is busy")]
    Busy,
    #[error("power controller error")]
    Controller,
}

pub trait Interface: DriverGeneric {
    fn power_on(&mut self, id: PowerDomainId) -> Result<(), PowerError>;

    fn power_off(&mut self, id: PowerDomainId) -> Result<(), PowerError>;

    fn is_powered(&self, _id: PowerDomainId) -> Result<bool, PowerError> {
        Err(PowerError::Unsupported)
    }
}

def_driver!(Power, Interface);

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingPower {
        powered: bool,
        calls: alloc::vec::Vec<(&'static str, PowerDomainId)>,
    }

    impl DriverGeneric for RecordingPower {
        fn name(&self) -> &str {
            "recording-power"
        }
    }

    impl Interface for RecordingPower {
        fn power_on(&mut self, id: PowerDomainId) -> Result<(), PowerError> {
            self.powered = true;
            self.calls.push(("on", id));
            Ok(())
        }

        fn power_off(&mut self, id: PowerDomainId) -> Result<(), PowerError> {
            self.powered = false;
            self.calls.push(("off", id));
            Ok(())
        }

        fn is_powered(&self, _id: PowerDomainId) -> Result<bool, PowerError> {
            Ok(self.powered)
        }
    }

    #[test]
    fn power_domain_id_conversions_preserve_raw_value() {
        assert_eq!(PowerDomainId::new(7).raw(), 7);
        assert_eq!(PowerDomainId::from(8_u32).raw(), 8);
        assert_eq!(PowerDomainId::from(9_usize).raw(), 9);
        assert_eq!(PowerDomainId::from(10_u64).raw(), 10);
    }

    #[test]
    fn power_domain_operations_are_dispatched_to_inner_driver() {
        let mut power = Power::new(RecordingPower {
            powered: false,
            calls: alloc::vec::Vec::new(),
        });

        power.power_on(PowerDomainId::new(3)).unwrap();
        assert_eq!(power.is_powered(PowerDomainId::new(3)), Ok(true));
        power.power_off(PowerDomainId::new(3)).unwrap();
        assert_eq!(power.is_powered(PowerDomainId::new(3)), Ok(false));

        let inner = power.typed_ref::<RecordingPower>().unwrap();
        assert_eq!(
            inner.calls,
            alloc::vec![
                ("on", PowerDomainId::new(3)),
                ("off", PowerDomainId::new(3))
            ]
        );
    }

    #[test]
    fn default_is_powered_reports_unsupported() {
        struct MinimalPower;

        impl DriverGeneric for MinimalPower {
            fn name(&self) -> &str {
                "minimal-power"
            }
        }

        impl Interface for MinimalPower {
            fn power_on(&mut self, _id: PowerDomainId) -> Result<(), PowerError> {
                Ok(())
            }

            fn power_off(&mut self, _id: PowerDomainId) -> Result<(), PowerError> {
                Ok(())
            }
        }

        let power = MinimalPower;
        assert_eq!(
            power.is_powered(PowerDomainId::new(1)),
            Err(PowerError::Unsupported)
        );
    }
}
