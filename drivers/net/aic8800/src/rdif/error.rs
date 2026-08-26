use sdmmc_protocol::Error as ProtocolError;

use crate::AicError;

/// Portable AIC RDIF adapter error.
#[derive(Debug, thiserror::Error)]
pub enum AicRdifError {
    /// SDIO card protocol failure.
    #[error("SDIO protocol failed: {0}")]
    Protocol(#[from] ProtocolError),
    /// AIC core state-machine failure.
    #[error("AIC core failed: {0}")]
    Core(#[from] AicError),
    /// The physical host did not expose the required DMA capability.
    #[error("SDIO host DMA capability is unavailable")]
    DmaUnavailable,
    /// A bounded ownership queue is full or disconnected.
    #[error("bounded AIC ownership queue is unavailable")]
    QueueUnavailable,
    /// The adapter was advanced after terminal shutdown.
    #[error("AIC adapter is stopped")]
    Stopped,
}

impl From<AicRdifError> for rdif_eth::NetError {
    fn from(error: AicRdifError) -> Self {
        match error {
            AicRdifError::QueueUnavailable => Self::Retry,
            AicRdifError::Stopped => Self::Stopped,
            AicRdifError::DmaUnavailable => Self::InvalidParts,
            other => Self::Other(alloc::boxed::Box::new(other)),
        }
    }
}

impl From<dma_api::DmaError> for AicRdifError {
    fn from(_: dma_api::DmaError) -> Self {
        Self::DmaUnavailable
    }
}
