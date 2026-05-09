#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScmiError {
    #[error("Not supported")]
    NotSupported,
    #[error("Invalid parameters")]
    InvalidParameters,
    #[error("Access denied")]
    AccessDenied,
    #[error("Not found")]
    NotFound,
    #[error("Value out of range")]
    OutOfRange,
    #[error("Device busy")]
    Busy,
    #[error("Communication error")]
    CommunicationError,
    #[error("Generic error")]
    GenericError,
    #[error("Hardware error")]
    HardwareError,
    #[error("Protocol error")]
    ProtocolError,
    #[error("Unknown SCMI error {0}")]
    Unknown(i32),
}

impl ScmiError {
    pub const SUCCESS: i32 = 0;

    pub fn from_status(status: i32) -> Result<(), Self> {
        match status {
            0 => Ok(()),
            -1 => Err(ScmiError::NotSupported),
            -2 => Err(ScmiError::InvalidParameters),
            -3 => Err(ScmiError::AccessDenied),
            -4 => Err(ScmiError::NotFound),
            -5 => Err(ScmiError::OutOfRange),
            -6 => Err(ScmiError::Busy),
            -7 => Err(ScmiError::CommunicationError),
            -8 => Err(ScmiError::GenericError),
            -9 => Err(ScmiError::HardwareError),
            -10 => Err(ScmiError::ProtocolError),
            other => Err(ScmiError::Unknown(other)),
        }
    }
}
