use core::fmt;

use sdmmc_protocol::Error as ProtocolError;

use crate::{AicError, ChipVariant};

/// Manufacturer tuple observed in one SDIO CIS chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AicSdioIdentity {
    /// Manufacturer identifier from `CISTPL_MANFID`, when present.
    pub manufacturer_id: Option<u16>,
    /// Product identifier from `CISTPL_MANFID`, when present.
    pub product_id: Option<u16>,
}

impl AicSdioIdentity {
    pub(crate) fn complete(self) -> Option<(u16, u16)> {
        self.manufacturer_id.zip(self.product_id)
    }
}

impl fmt::Display for AicSdioIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.manufacturer_id, self.product_id) {
            (Some(manufacturer_id), Some(product_id)) => {
                write!(formatter, "{manufacturer_id:04x}:{product_id:04x}")
            }
            (Some(manufacturer_id), None) => {
                write!(formatter, "{manufacturer_id:04x}:missing")
            }
            (None, Some(product_id)) => write!(formatter, "missing:{product_id:04x}"),
            (None, None) => formatter.write_str("missing"),
        }
    }
}

/// Portable AIC RDIF adapter error.
#[derive(Debug, thiserror::Error)]
pub enum AicRdifError {
    /// SDIO card protocol failure.
    #[error("SDIO protocol failed: {0}")]
    Protocol(#[from] ProtocolError),
    /// AIC core state-machine failure.
    #[error("AIC core failed: {0}")]
    Core(#[from] AicError),
    /// The card CIS did not identify a supported AIC variant.
    #[error(
        "unsupported AIC SDIO identity: detected {detected:?}, function1={function1}, \
         common={common}, io_functions={io_functions}"
    )]
    UnsupportedCardIdentity {
        /// Variant derived from the effective Linux-style CIS identity.
        detected: ChipVariant,
        /// Number of I/O functions reported by CMD5.
        io_functions: u8,
        /// Function-one CIS identity.
        function1: AicSdioIdentity,
        /// Common CIS identity.
        common: AicSdioIdentity,
    },
    /// The owner attempted core work before SDIO identity validation completed.
    #[error("AIC core is unavailable before SDIO card identification")]
    CoreUnavailable,
    /// End-to-end owner startup expired before the card and firmware became ready.
    #[error(
        "AIC startup timed out: enumeration_started={enumeration_started}, \
         card_protocol_ready={card_protocol_ready}, irq_sequence={irq_sequence}, \
         irq_pending={irq_pending}, completion_pending={completion_pending:?}"
    )]
    StartupTimeout {
        /// Whether SDIO card enumeration had been submitted.
        enumeration_started: bool,
        /// Whether CIS validation completed and the AIC core was constructed.
        card_protocol_ready: bool,
        /// Last hard-IRQ publication sequence observed by the owner latch.
        irq_sequence: u64,
        /// Whether an IRQ fact remained unconsumed at the deadline.
        irq_pending: bool,
        /// Whether task-context completion rearm found already-latched status;
        /// `None` means the host could not provide a diagnostic readback.
        completion_pending: Option<bool>,
    },
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
