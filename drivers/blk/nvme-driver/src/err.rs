use core::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum Error {
    NoMemory,
    Layout,
    Dma(dma_api::DmaError),
    Mmio(mmio_api::MapError),
    Activation(rdif_block::ActivationError),
    Unknown(&'static str),
}

pub type Result<T = ()> = core::result::Result<T, Error>;

impl From<dma_api::DmaError> for Error {
    fn from(value: dma_api::DmaError) -> Self {
        match value {
            dma_api::DmaError::NoMemory => Self::NoMemory,
            dma_api::DmaError::LayoutError(_) => Self::Layout,
            other => Self::Dma(other),
        }
    }
}

impl From<mmio_api::MapError> for Error {
    fn from(value: mmio_api::MapError) -> Self {
        Self::Mmio(value)
    }
}

impl From<rdif_block::ActivationError> for Error {
    fn from(value: rdif_block::ActivationError) -> Self {
        Self::Activation(value)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoMemory => f.write_str("no memory available"),
            Self::Layout => f.write_str("invalid memory layout"),
            Self::Dma(err) => write!(f, "dma error: {err}"),
            Self::Mmio(err) => write!(f, "mmio map error: {err}"),
            Self::Activation(err) => write!(f, "block activation contract error: {err}"),
            Self::Unknown(message) => f.write_str(message),
        }
    }
}

impl core::error::Error for Error {}
