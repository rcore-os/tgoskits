use alloc::{boxed::Box, string::String};
use core::fmt;

use crate::{FirmwareKind, FunctionId, GpioLineId, GroupId, PinId, io};

#[derive(thiserror::Error, Debug)]
pub enum PinctrlError {
    #[error("operation not supported")]
    NotSupported,
    #[error("firmware source is not supported: {0:?}")]
    UnsupportedFirmware(FirmwareKind),
    #[error("device is not available")]
    NotAvailable,
    #[error("invalid pin: {0:?}")]
    InvalidPin(PinId),
    #[error("invalid group: {0:?}")]
    InvalidGroup(GroupId),
    #[error("invalid function: {0:?}")]
    InvalidFunction(FunctionId),
    #[error("function {function:?} cannot mux group {group:?}")]
    InvalidMux {
        group: GroupId,
        function: FunctionId,
    },
    #[error("invalid GPIO line: {0:?}")]
    InvalidLine(GpioLineId),
    #[error("GPIO line is already requested: {0:?}")]
    LineBusy(GpioLineId),
    #[error("GPIO line is not requested: {0:?}")]
    LineNotRequested(GpioLineId),
    #[error("invalid pin configuration")]
    InvalidConfig,
    #[error("IRQ event overflow")]
    IrqEventOverflow,
    #[error("other error: {0}")]
    Other(Box<dyn core::error::Error>),
}

impl PartialEq for PinctrlError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::NotSupported, Self::NotSupported)
            | (Self::NotAvailable, Self::NotAvailable)
            | (Self::InvalidConfig, Self::InvalidConfig)
            | (Self::IrqEventOverflow, Self::IrqEventOverflow) => true,
            (Self::UnsupportedFirmware(a), Self::UnsupportedFirmware(b)) => a == b,
            (Self::InvalidPin(a), Self::InvalidPin(b)) => a == b,
            (Self::InvalidGroup(a), Self::InvalidGroup(b)) => a == b,
            (Self::InvalidFunction(a), Self::InvalidFunction(b)) => a == b,
            (
                Self::InvalidMux {
                    group: a_group,
                    function: a_function,
                },
                Self::InvalidMux {
                    group: b_group,
                    function: b_function,
                },
            ) => a_group == b_group && a_function == b_function,
            (Self::InvalidLine(a), Self::InvalidLine(b))
            | (Self::LineBusy(a), Self::LineBusy(b))
            | (Self::LineNotRequested(a), Self::LineNotRequested(b)) => a == b,
            (Self::Other(_), Self::Other(_)) => false,
            _ => false,
        }
    }
}

impl Eq for PinctrlError {}

#[derive(Debug)]
struct PinctrlMessageError(String);

impl fmt::Display for PinctrlMessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for PinctrlMessageError {}

impl PinctrlError {
    pub fn other(message: impl Into<String>) -> Self {
        Self::Other(Box::new(PinctrlMessageError(message.into())))
    }
}

impl From<PinctrlError> for io::ErrorKind {
    fn from(value: PinctrlError) -> Self {
        match value {
            PinctrlError::NotSupported | PinctrlError::UnsupportedFirmware(_) => {
                io::ErrorKind::Unsupported
            }
            PinctrlError::NotAvailable => io::ErrorKind::NotAvailable,
            PinctrlError::InvalidPin(_)
            | PinctrlError::InvalidGroup(_)
            | PinctrlError::InvalidFunction(_)
            | PinctrlError::InvalidMux { .. }
            | PinctrlError::InvalidLine(_)
            | PinctrlError::InvalidConfig => io::ErrorKind::InvalidData,
            PinctrlError::LineBusy(_) => io::ErrorKind::Interrupted,
            PinctrlError::LineNotRequested(_) | PinctrlError::IrqEventOverflow => {
                io::ErrorKind::Other(Box::new(value))
            }
            PinctrlError::Other(error) => io::ErrorKind::Other(error),
        }
    }
}
