#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("DMA error: {0}")]
    Dma(#[from] dma_api::DmaError),

    #[error("MMIO map error: {0}")]
    Mmio(#[from] mmio_api::MapError),

    #[error("MMIO mapping is too small: {size:#x} < {required:#x}")]
    MmioTooSmall { size: usize, required: usize },

    #[error("unsupported device")]
    Unsupported,

    #[error("link down")]
    LinkDown,

    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),

    #[error("invalid hardware MAC address {0:02x?}")]
    InvalidMacAddress([u8; 6]),

    #[error("operation timeout")]
    Timeout,

    #[error("other: {0}")]
    Other(&'static str),
}

pub type Result<T = ()> = core::result::Result<T, Error>;
