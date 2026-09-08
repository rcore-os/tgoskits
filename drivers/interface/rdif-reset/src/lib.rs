#![no_std]

extern crate alloc;

use rdif_base::def_driver;
pub use rdif_base::{DriverGeneric, custom_type};

custom_type!(
    #[doc = "Reset signal id"],
    ResetId, u64, "{:#x}");

impl ResetId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

impl From<u32> for ResetId {
    fn from(value: u32) -> Self {
        Self(u64::from(value))
    }
}

impl From<usize> for ResetId {
    fn from(value: usize) -> Self {
        Self(value as u64)
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetError {
    #[error("invalid reset id")]
    InvalidId,
    #[error("unsupported reset operation")]
    Unsupported,
    #[error("reset controller is busy")]
    Busy,
    #[error("reset controller error")]
    Controller,
}

pub trait Interface: DriverGeneric {
    fn assert(&mut self, id: ResetId) -> Result<(), ResetError>;

    fn deassert(&mut self, id: ResetId) -> Result<(), ResetError>;

    fn reset(&mut self, id: ResetId) -> Result<(), ResetError> {
        self.assert(id)?;
        self.deassert(id)
    }
}

def_driver!(Reset, Interface);

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingReset {
        calls: alloc::vec::Vec<(&'static str, ResetId)>,
    }

    impl DriverGeneric for RecordingReset {
        fn name(&self) -> &str {
            "recording-reset"
        }
    }

    impl Interface for RecordingReset {
        fn assert(&mut self, id: ResetId) -> Result<(), ResetError> {
            self.calls.push(("assert", id));
            Ok(())
        }

        fn deassert(&mut self, id: ResetId) -> Result<(), ResetError> {
            self.calls.push(("deassert", id));
            Ok(())
        }
    }

    #[test]
    fn reset_pulses_assert_then_deassert() {
        let mut reset = RecordingReset {
            calls: alloc::vec::Vec::new(),
        };

        reset.reset(ResetId::new(3)).unwrap();

        assert_eq!(
            reset.calls,
            alloc::vec![("assert", ResetId::new(3)), ("deassert", ResetId::new(3))]
        );
    }
}
