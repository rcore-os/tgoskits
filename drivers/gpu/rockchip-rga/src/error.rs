//! Driver-core error type. Maps to UAPI errno at the `/dev/rga` layer later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RgaError {
    #[error("invalid RGA operation")]
    Invalid,
    #[error("RGA value overflow")]
    Overflow,
    #[error("RGA operation is not supported")]
    Unsupported,
    #[error("RGA operation timed out")]
    Timeout,
    #[error("RGA hardware failure")]
    Hardware,
    #[error("RGA device is busy")]
    Busy,
    #[error("RGA DMA operation failed")]
    Dma,
}

pub type Result<T> = core::result::Result<T, RgaError>;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn errors_are_copy_and_eq() {
        assert_eq!(RgaError::Timeout, RgaError::Timeout);
        let e = RgaError::Unsupported;
        let _copy = e; // Copy
        assert_ne!(e, RgaError::Invalid);
    }
}
