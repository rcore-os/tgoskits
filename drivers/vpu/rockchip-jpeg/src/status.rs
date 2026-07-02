//! Decoding of the JPEG decoder `SWREG1` interrupt/status word.

use crate::registers;

/// A decode error reported by the hardware in `SWREG1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// AXI bus error (`sw_dec_bus_sta`).
    BusError,
    /// Stream decode error (`sw_dec_error_sta`).
    StreamError,
    /// Decode timed out in hardware (`sw_dec_timeout_sta`).
    Timeout,
    /// Input stream buffer ran empty (`sw_dec_buf_empty_sta`).
    BufferEmpty,
}

/// Decoded view of the `SWREG1` interrupt/status register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeStatus {
    raw: u32,
}

impl DecodeStatus {
    /// Wrap a raw `SWREG1` value.
    pub const fn from_int(reg1: u32) -> Self {
        Self { raw: reg1 }
    }

    /// Raw register value.
    pub const fn raw(self) -> u32 {
        self.raw
    }

    /// Whether the frame-ready (done) bit is set.
    pub const fn is_done(self) -> bool {
        self.raw & registers::INT_RDY_STA != 0
    }

    /// The first hardware error reported, if any (checked most- to least-severe).
    pub const fn error(self) -> Option<DecodeError> {
        if self.raw & registers::INT_BUS_STA != 0 {
            Some(DecodeError::BusError)
        } else if self.raw & registers::INT_ERROR_STA != 0 {
            Some(DecodeError::StreamError)
        } else if self.raw & registers::INT_TIMEOUT_STA != 0 {
            Some(DecodeError::Timeout)
        } else if self.raw & registers::INT_BUF_EMPTY_STA != 0 {
            Some(DecodeError::BufferEmpty)
        } else {
            None
        }
    }

    /// Whether decoding completed successfully (done and no error).
    pub const fn is_success(self) -> bool {
        self.is_done() && self.raw & registers::INT_ERROR_MASK == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_with_no_error_is_success() {
        let s = DecodeStatus::from_int(registers::INT_RDY_STA | registers::INT_IRQ);
        assert!(s.is_done());
        assert_eq!(s.error(), None);
        assert!(s.is_success());
    }

    #[test]
    fn stream_error_is_reported_and_not_success() {
        let s = DecodeStatus::from_int(registers::INT_ERROR_STA);
        assert_eq!(s.error(), Some(DecodeError::StreamError));
        assert!(!s.is_success());
    }

    #[test]
    fn bus_error_is_reported() {
        let s = DecodeStatus::from_int(registers::INT_BUS_STA);
        assert_eq!(s.error(), Some(DecodeError::BusError));
    }

    #[test]
    fn timeout_is_reported() {
        let s = DecodeStatus::from_int(registers::INT_TIMEOUT_STA);
        assert_eq!(s.error(), Some(DecodeError::Timeout));
    }

    #[test]
    fn buffer_empty_is_reported() {
        let s = DecodeStatus::from_int(registers::INT_BUF_EMPTY_STA);
        assert_eq!(s.error(), Some(DecodeError::BufferEmpty));
    }

    #[test]
    fn not_done_is_not_success() {
        let s = DecodeStatus::from_int(0);
        assert!(!s.is_done());
        assert!(!s.is_success());
    }

    #[test]
    fn error_overrides_done_for_success() {
        let s = DecodeStatus::from_int(registers::INT_RDY_STA | registers::INT_ERROR_STA);
        assert!(s.is_done());
        assert_eq!(s.error(), Some(DecodeError::StreamError));
        assert!(!s.is_success());
    }
}
