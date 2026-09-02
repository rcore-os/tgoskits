use thiserror::Error;

use super::IvcMessageId;

/// Errors produced while sending or receiving an IVC logical message.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum IvcMessageError {
    /// The cell ring has no free entry for the requested operation.
    #[error("the IVC cell ring is full")]
    CellFull,
    /// A new message was requested while another message is still being sent.
    #[error("an IVC message send is already in progress")]
    SendInProgress,
    /// The operation requires an active outgoing message.
    #[error("no IVC message send is in progress")]
    NoMessageInProgress,
    /// The supplied input is longer than the unsent part of the message.
    #[error("input has {provided} bytes but only {remaining} message bytes remain")]
    InputExceedsRemaining {
        /// Number of bytes not yet sent.
        remaining: u64,
        /// Number of bytes supplied by the caller.
        provided: usize,
    },
    /// This sender has used every nonzero V1 message identifier.
    #[error("the IVC message identifier space is exhausted")]
    MessageIdExhausted,
    /// The frame uses a message protocol version unsupported by this crate.
    #[error("unsupported IVC message version {version}")]
    UnsupportedVersion {
        /// Version byte observed in the cell.
        version: u8,
    },
    /// The frame contains flag bits unknown to this protocol version.
    #[error("unknown IVC message flags {flags:#04x}")]
    UnknownFlags {
        /// Raw flags byte observed in the cell.
        flags: u8,
    },
    /// The frame header or its flag/length combination is invalid.
    #[error("malformed IVC message frame header")]
    MalformedHeader,
    /// A frame without `FIRST` was observed while no message was active.
    #[error("IVC message frame is missing FIRST")]
    MissingFirst,
    /// A second `FIRST` frame was observed during an active message.
    #[error("unexpected FIRST in an active IVC message")]
    UnexpectedFirst,
    /// A fragment does not belong to the active message.
    #[error("expected IVC message {expected:?}, received {actual:?}")]
    UnexpectedMessageId {
        /// Active transport message identifier.
        expected: IvcMessageId,
        /// Identifier found in the frame.
        actual: IvcMessageId,
    },
    /// A fragment changed the declared total message length.
    #[error("expected IVC message length {expected}, received {actual}")]
    InconsistentMessageLength {
        /// Length declared by the first frame.
        expected: u64,
        /// Length declared by the inconsistent frame.
        actual: u64,
    },
    /// A frame declares more fragment bytes than its cell can hold.
    #[error("IVC fragment length {length} exceeds cell capacity {capacity}")]
    FragmentTooLarge {
        /// Declared fragment length.
        length: usize,
        /// Maximum fragment capacity for the frame.
        capacity: usize,
    },
    /// Received fragments exceed the declared message length.
    #[error("received {received} bytes for an IVC message declared as {declared} bytes")]
    MessageLengthExceeded {
        /// Length declared by the message.
        declared: u64,
        /// Length after accepting the offending fragment.
        received: u64,
    },
    /// A `LAST` frame did not end at the declared message length.
    #[error("LAST ended at {actual} bytes, expected {expected}")]
    LengthMismatchAtLast {
        /// Declared message length.
        expected: u64,
        /// Accumulated length at `LAST`.
        actual: u64,
    },
    /// The output cannot hold the next complete cell fragment.
    #[error("output has {provided} bytes but the next fragment requires {required}")]
    BufferTooSmall {
        /// Space required for the next fragment.
        required: usize,
        /// Space currently available.
        provided: usize,
    },
    /// The peer explicitly aborted the active message.
    #[error("the peer aborted the active IVC message")]
    TransferAborted,
    /// The cell transport reset while a message was active.
    #[error("the IVC peer reset the cell transport")]
    PeerReset,
}
