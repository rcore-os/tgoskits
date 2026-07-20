#![no_std]

extern crate alloc;

use rdif_base::def_driver;
pub use rdif_base::{DriverGeneric, KError, custom_type};

custom_type!(
    #[doc = "Clock signal id"],
    ClockId, usize, "{:#x}");

pub trait Interface: DriverGeneric {
    fn perper_enable(&mut self);

    fn enable(&mut self, _id: ClockId) -> Result<(), KError> {
        Ok(())
    }

    /// Returns whether the selected clock is currently ungated.
    fn is_enabled(&self, _id: ClockId) -> Result<bool, KError> {
        Err(KError::Unsupported {
            operation: "clock enable-state query",
        })
    }

    fn get_rate(&self, id: ClockId) -> Result<u64, KError>;

    fn set_rate(&mut self, id: ClockId, rate: u64) -> Result<(), KError>;
}

def_driver!(Clk, Interface);

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedClock;

    impl DriverGeneric for FixedClock {
        fn name(&self) -> &str {
            "fixed-clock"
        }
    }

    impl Interface for FixedClock {
        fn perper_enable(&mut self) {}

        fn get_rate(&self, _id: ClockId) -> Result<u64, KError> {
            Ok(24_000_000)
        }

        fn set_rate(&mut self, _id: ClockId, _rate: u64) -> Result<(), KError> {
            Err(KError::Unsupported {
                operation: "fixed clock rate change",
            })
        }
    }

    #[test]
    fn enable_state_query_is_explicitly_unsupported_by_default() {
        let clock = FixedClock;

        assert_eq!(
            clock.is_enabled(ClockId::from(0_usize)),
            Err(KError::Unsupported {
                operation: "clock enable-state query"
            })
        );
    }
}
