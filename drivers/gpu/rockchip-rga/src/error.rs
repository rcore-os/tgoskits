//! Driver-core error type. Maps to UAPI errno at the `/dev/rga` layer later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgaError {
    Invalid,
    Overflow,
    Unsupported,
    Timeout,
    Hardware,
    Busy,
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
