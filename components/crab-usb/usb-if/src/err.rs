use alloc::{boxed::Box, string::String};

#[derive(thiserror::Error, Debug)]
pub enum TransferError {
    #[error("Stall")]
    Stall,
    #[error("Queue full")]
    QueueFull,
    #[error("Invalid endpoint")]
    InvalidEndpoint,
    #[error("No device")]
    NoDevice,
    #[error("Not supported")]
    NotSupported,
    #[error("Timeout")]
    Timeout,
    #[error("Cancelled")]
    Cancelled,
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}

impl From<Box<dyn core::error::Error>> for TransferError {
    fn from(err: Box<dyn core::error::Error>) -> Self {
        TransferError::Other(anyhow::anyhow!("{}", err))
    }
}

#[derive(thiserror::Error, Debug)]
pub enum USBError {
    #[error("Timeout")]
    Timeout,
    #[error("No memory available")]
    NoMemory,
    #[error("Transfer error: {0}")]
    TransferError(#[from] TransferError),
    #[error("Not initialized")]
    NotInitialized,
    #[error("Not found")]
    NotFound,
    #[error("Invalid parameter")]
    InvalidParameter,
    #[error("Slot limit reached")]
    SlotLimitReached,
    #[error("Configuration not set")]
    ConfigurationNotSet,
    #[error("Not supported")]
    NotSupported,
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}

impl From<&str> for USBError {
    fn from(value: &str) -> Self {
        USBError::Other(anyhow::anyhow!("{value}"))
    }
}

impl From<String> for USBError {
    fn from(value: String) -> Self {
        USBError::Other(anyhow::anyhow!(value))
    }
}

/*

LIBUSB_SUCCESS
Success (no error)

LIBUSB_ERROR_IO
Input/output error.

LIBUSB_ERROR_INVALID_PARAM
Invalid parameter.

LIBUSB_ERROR_ACCESS
Access denied (insufficient permissions)

LIBUSB_ERROR_NO_DEVICE
No such device (it may have been disconnected)

LIBUSB_ERROR_NOT_FOUND
Entity not found.

LIBUSB_ERROR_BUSY
Resource busy.

LIBUSB_ERROR_TIMEOUT
Operation timed out.

LIBUSB_ERROR_OVERFLOW
Overflow.

LIBUSB_ERROR_PIPE
Pipe error.

LIBUSB_ERROR_INTERRUPTED
System call interrupted (perhaps due to signal)

LIBUSB_ERROR_NO_MEM
Insufficient memory.

LIBUSB_ERROR_NOT_SUPPORTED
Operation not supported or unimplemented on this platform.

LIBUSB_ERROR_OTHER
Other error.
*/
