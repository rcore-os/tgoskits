use alloc::boxed::Box;

use rdif_gpu::GpuError;

use crate::io;

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayError {
    #[error("operation not supported")]
    Unsupported,
    #[error("display is not available")]
    NotAvailable,
    #[error("invalid output")]
    InvalidOutput,
    #[error("invalid display state")]
    InvalidState,
    #[error("display is busy")]
    Busy,
    #[error("display is not ready")]
    NotReady,
    #[error("display device was lost")]
    DeviceLost,
    #[error("display I/O failed")]
    Io,
    #[error("GPU operation failed: {0}")]
    Gpu(#[from] GpuError),
}

impl From<DisplayError> for io::ErrorKind {
    fn from(value: DisplayError) -> Self {
        match value {
            DisplayError::Unsupported | DisplayError::Gpu(GpuError::Unsupported) => {
                Self::Unsupported
            }
            DisplayError::NotAvailable
            | DisplayError::DeviceLost
            | DisplayError::Gpu(GpuError::NotAvailable | GpuError::DeviceLost) => {
                Self::NotAvailable
            }
            DisplayError::InvalidOutput
            | DisplayError::InvalidState
            | DisplayError::Gpu(GpuError::InvalidArgument | GpuError::InvalidHandle) => {
                Self::InvalidData
            }
            DisplayError::Gpu(GpuError::OutOfMemory) => Self::OutOfMemory,
            error => Self::Other(Box::new(error)),
        }
    }
}
