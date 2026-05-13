//! Error and diagnostic context types returned by drivers and parsers.
//!
//! See [`Phase`] and [`ErrorContext`] for the operational metadata that
//! recoverable [`Error`] variants carry.

/// Where in the driver pipeline a fault was observed.
///
/// Attached to recoverable [`Error`] variants via [`ErrorContext`] so callers
/// can distinguish e.g. a CMD0 send timeout from a `BusyWait` programming
/// timeout without parsing log strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No response from card within the deadline for the wrapped phase.
    Timeout(ErrorContext),
    /// CRC check failed during the wrapped phase.
    Crc(ErrorContext),
    /// Card is not responding or not inserted.
    NoCard,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardError {
    /// A command was issued out of sequence.
    IllegalCommand,
    /// CRC check of the last command failed.
    CommandCrcFailed,
    /// Erase sequence error.
    EraseSequence,
    /// Address alignment error.
    AddressError,
    /// Card internal ECC error.
    CardEccFailed,
    /// Generic controller error.
    ControllerError,
    /// Unknown error bit set.
    Unknown(u8),
}
