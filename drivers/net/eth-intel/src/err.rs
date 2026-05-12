#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("DMA error: {0}")]
    Dma(#[from] dma_api::DmaError),

    #[error("MMIO map error: {0}")]
    Mmio(#[from] mmio_api::MapError),

    #[error("unsupported device")]
    Unsupported,

    #[error("link down")]
    LinkDown,

    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),

    #[error("operation timeout")]
    Timeout,

    #[error("other: {0}")]
    Other(&'static str),
}

pub type Result<T = ()> = core::result::Result<T, Error>;
