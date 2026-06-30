//! Error and diagnostic context types returned by drivers and parsers.
//!
//! See [`Phase`] and [`ErrorContext`] for the operational metadata that
//! recoverable [`Error`] variants carry.
//!
//! All public error types implement [`core::fmt::Display`] for human-readable
//! logging, and [`Error`] additionally implements [`core::error::Error`]
//! (stabilized in `no_std` since Rust 1.81) so it composes with
//! `?`-propagation chains and downstream error-handling utilities. The
//! `Debug` impls are still derived for `{:?}` use inside the driver.

use core::fmt;

/// Where in the driver pipeline a fault was observed.
///
/// Attached to recoverable [`Error`] variants via [`ErrorContext`] so callers
/// can distinguish e.g. a CMD0 send timeout from a `BusyWait` programming
/// timeout without parsing log strings.
///
/// Marked `#[non_exhaustive]`: more phases (e.g. tuning, voltage switch) are
/// expected to land before 1.0, and downstream `match` sites must keep a
/// `_ => ...` arm to absorb them without recompiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Phase {
    /// Phase was not recorded.
    ///
    /// Used as a default placeholder; real driver paths should pick a
    /// concrete variant.
    #[default]
    Unspecified,
    /// Power-up / running CMD0 → ACMD41 / sending CMD2/3/9/7.
    Init,
    /// Putting the command bytes onto the bus.
    CommandSend,
    /// Waiting for the card's response token / R1–R7 payload.
    ResponseWait,
    /// Streaming a data block to the card (CMD24 / CMD25 etc).
    DataWrite,
    /// Streaming a data block from the card (CMD17 / CMD18 etc).
    DataRead,
    /// Polling the card's busy line / programming status.
    BusyWait,
    /// Switching bus speed, width or function (CMD6 / ACMD6).
    Switch,
    /// Erase sequence (CMD32 / CMD33 / CMD38).
    Erase,
}

/// Operational context attached to recoverable bus / protocol errors.
///
/// Helps callers triage failures: which phase of the SD/MMC pipeline
/// raised the error, and which CMD/ACMD index was being processed at
/// the time, when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ErrorContext {
    /// Pipeline phase when the fault was raised.
    pub phase: Phase,
    /// CMD/ACMD index being processed, if applicable.
    pub cmd: Option<u8>,
}

impl ErrorContext {
    /// Build a context with only the phase populated.
    #[inline]
    pub const fn new(phase: Phase) -> Self {
        Self { phase, cmd: None }
    }

    /// Build a context tied to a specific CMD/ACMD index.
    #[inline]
    pub const fn for_cmd(phase: Phase, cmd: u8) -> Self {
        Self {
            phase,
            cmd: Some(cmd),
        }
    }
}

/// Errors returned by SD/MMC protocol parsers and drivers.
///
/// Recoverable bus / protocol variants carry an [`ErrorContext`] pinpointing
/// the phase and (when known) command index that raised them. Caller-facing
/// programming errors (`Misaligned`, `InvalidArgument`) and card-state
/// reports (`NoCard`, `CardError`, `CardLocked`) do not.
///
/// Marked `#[non_exhaustive]`: more variants (e.g. `NoCardDetected`,
/// `VoltageSwitchFailed`, retry-exhausted) are expected before 1.0. Match
/// sites in downstream crates must keep a `_ => ...` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// No response from card within the deadline for the wrapped phase.
    Timeout(ErrorContext),
    /// CRC check failed during the wrapped phase.
    Crc(ErrorContext),
    /// Card is not responding or not inserted.
    NoCard,
    /// Host/controller currently has another active request.
    Busy,
    /// Command index is not supported on this transport.
    UnsupportedCommand,
    /// Bad response received during the wrapped phase.
    BadResponse(ErrorContext),
    /// Card returned an error in its R1 response.
    CardError(CardError),
    /// Write operation failed during the wrapped phase.
    WriteError(ErrorContext),
    /// Read operation failed during the wrapped phase.
    ReadError(ErrorContext),
    /// Misaligned address or length passed by the caller.
    Misaligned,
    /// Caller passed an invalid argument.
    InvalidArgument,
    /// Card is locked (host needs to unlock before further I/O).
    CardLocked,
    /// Generic communication error on the bus during the wrapped phase.
    BusError(ErrorContext),
}

/// Per-bit error status decoded out of an R1 response.
///
/// SD Physical Layer spec section 4.10.1 reserves bits 19..=31 of the 32-bit
/// native R1 response for card-state error flags. SPI R1 reuses bits 1..=6 of
/// the single response byte for a subset of those. Variants below cover both,
/// with [`CardError::Unknown`] preserving the raw native bit pattern when no
/// known flag matches (e.g. reserved-for-application bits).
///
/// Marked `#[non_exhaustive]`: new card-status bits may be classified out of
/// `Unknown(_)` over time, and downstream match sites must keep a `_ => ...`
/// arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CardError {
    /// `OUT_OF_RANGE` (bit 31): the command's argument was out of the allowed
    /// range for this card (e.g. LBA beyond capacity).
    OutOfRange,
    /// `ADDRESS_ERROR` (bit 30) / SPI `ADDRESS_ERROR` (bit 5): misaligned
    /// address for the current block length, or out-of-range address.
    AddressError,
    /// `BLOCK_LEN_ERROR` (bit 29) / SPI `PARAMETER_ERROR` (bit 6): transferred
    /// block length is not allowed for this card or the parameter argument
    /// was out of range.
    BlockLenError,
    /// `ERASE_SEQ_ERROR` (bit 28) / SPI `ERASE_SEQ_ERROR` (bit 4): erase
    /// command sequence error, or `ERASE_RESET` (SPI bit 1).
    EraseSequence,
    /// `ERASE_PARAM` (bit 27): an invalid selection of write blocks for erase.
    EraseParam,
    /// `WP_VIOLATION` (bit 26): attempted write to a write-protected block.
    WriteProtect,
    /// `CARD_IS_LOCKED` (bit 25): card is locked by host, normal data
    /// transfers are inhibited.
    CardIsLocked,
    /// `LOCK_UNLOCK_FAILED` (bit 24): a sequence or password error in the
    /// lock/unlock command.
    LockUnlockFailed,
    /// `COM_CRC_ERROR` (bit 23) / SPI `COM_CRC_ERROR` (bit 3): CRC check of
    /// the previous command failed.
    CommandCrcFailed,
    /// `ILLEGAL_COMMAND` (bit 22) / SPI `ILLEGAL_COMMAND` (bit 2): command not
    /// legal for the current card state.
    IllegalCommand,
    /// `CARD_ECC_FAILED` (bit 21): card internal ECC was applied but failed
    /// to correct the data.
    CardEccFailed,
    /// `CC_ERROR` (bit 20): generic card controller error.
    ControllerError,
    /// `ERROR` (bit 19): a catch-all reported by the card when a non-classified
    /// internal error occurred during the command execution.
    GenericError,
    /// Unknown / reserved error bit set. Carries the native 13-bit error
    /// nibble (`raw >> 19`) so the caller can log the exact pattern.
    Unknown(u32),
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Unspecified => "unspecified phase",
            Self::Init => "init",
            Self::CommandSend => "command send",
            Self::ResponseWait => "response wait",
            Self::DataWrite => "data write",
            Self::DataRead => "data read",
            Self::BusyWait => "busy wait",
            Self::Switch => "switch",
            Self::Erase => "erase",
        };
        f.write_str(s)
    }
}

impl fmt::Display for ErrorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.cmd {
            Some(cmd) => write!(f, "{} (CMD{cmd})", self.phase),
            None => fmt::Display::fmt(&self.phase, f),
        }
    }
}

impl fmt::Display for CardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::OutOfRange => "out-of-range argument",
            Self::AddressError => "misaligned address",
            Self::BlockLenError => "invalid block length",
            Self::EraseSequence => "erase sequence error",
            Self::EraseParam => "invalid erase selection",
            Self::WriteProtect => "write-protect violation",
            Self::CardIsLocked => "card is locked",
            Self::LockUnlockFailed => "lock/unlock command failed",
            Self::CommandCrcFailed => "command CRC failed",
            Self::IllegalCommand => "illegal command for current card state",
            Self::CardEccFailed => "card internal ECC failed",
            Self::ControllerError => "card controller error",
            Self::GenericError => "generic card error",
            Self::Unknown(bits) => return write!(f, "unknown card error bits {bits:#x}"),
        };
        f.write_str(s)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout(ctx) => write!(f, "timeout during {ctx}"),
            Self::Crc(ctx) => write!(f, "CRC mismatch during {ctx}"),
            Self::NoCard => f.write_str("no card present"),
            Self::Busy => f.write_str("host controller is busy"),
            Self::UnsupportedCommand => f.write_str("command not supported by transport"),
            Self::BadResponse(ctx) => write!(f, "bad response during {ctx}"),
            Self::CardError(err) => write!(f, "card reported {err}"),
            Self::WriteError(ctx) => write!(f, "write failed during {ctx}"),
            Self::ReadError(ctx) => write!(f, "read failed during {ctx}"),
            Self::Misaligned => f.write_str("misaligned address or length"),
            Self::InvalidArgument => f.write_str("invalid argument"),
            Self::CardLocked => f.write_str("card is locked; unlock before further I/O"),
            Self::BusError(ctx) => write!(f, "bus error during {ctx}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::CardError(err) => Some(err),
            _ => None,
        }
    }
}

impl core::error::Error for CardError {}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::format;

    use super::*;

    #[test]
    fn display_error_includes_phase_and_cmd() {
        let err = Error::Timeout(ErrorContext::for_cmd(Phase::DataRead, 17));
        assert_eq!(format!("{err}"), "timeout during data read (CMD17)");
    }

    #[test]
    fn display_error_without_cmd_drops_parenthesis() {
        let err = Error::BadResponse(ErrorContext::new(Phase::ResponseWait));
        assert_eq!(format!("{err}"), "bad response during response wait");
    }

    #[test]
    fn display_card_error_known_variant() {
        let err = Error::CardError(CardError::OutOfRange);
        assert_eq!(format!("{err}"), "card reported out-of-range argument");
    }

    #[test]
    fn display_card_error_unknown_preserves_bits() {
        let err = Error::CardError(CardError::Unknown(0x1234));
        assert_eq!(
            format!("{err}"),
            "card reported unknown card error bits 0x1234"
        );
    }

    #[test]
    fn error_trait_source_threads_card_error_through() {
        let err = Error::CardError(CardError::WriteProtect);
        let src = core::error::Error::source(&err).expect("source should be CardError");
        assert_eq!(format!("{src}"), "write-protect violation");
    }

    #[test]
    fn error_trait_source_is_none_for_bus_errors() {
        let err = Error::Crc(ErrorContext::for_cmd(Phase::DataRead, 18));
        assert!(core::error::Error::source(&err).is_none());
    }
}
