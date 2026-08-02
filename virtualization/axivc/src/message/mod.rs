//! Fragmented logical messages transported over fixed-size opaque cells.
//!
//! Applications provide only message payload bytes. This module assigns
//! transport identifiers and preserves message boundaries with private V1
//! `FIRST`, `LAST`, and `ABORT` frames.

mod error;
mod frame;
mod receiver;
mod sender;

pub use error::IvcMessageError;
pub use receiver::IvcMessageReceiver;
pub use sender::IvcMessageSender;

/// Nonzero transport identifier assigned to one logical message.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct IvcMessageId(u64);

impl IvcMessageId {
    pub(crate) const fn new(raw: u64) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    /// Returns the wire value of this identifier.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Metadata declared by the first frame of a logical message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcMessageMeta {
    id: IvcMessageId,
    len: u64,
}

impl IvcMessageMeta {
    pub(crate) const fn new(id: IvcMessageId, len: u64) -> Self {
        Self { id, len }
    }

    /// Returns the transport-level message identifier.
    pub const fn id(self) -> IvcMessageId {
        self.id
    }

    /// Returns the complete logical payload length.
    pub const fn len(self) -> u64 {
        self.len
    }

    /// Returns whether the logical payload is empty.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// Progress made by one nonblocking send attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcSendProgress {
    consumed: usize,
    published_cells: usize,
    complete: bool,
}

impl IvcSendProgress {
    pub(crate) const fn new(consumed: usize, published_cells: usize, complete: bool) -> Self {
        Self {
            consumed,
            published_cells,
            complete,
        }
    }

    /// Returns the number of input bytes encoded and published.
    pub const fn consumed(self) -> usize {
        self.consumed
    }

    /// Returns the number of cells published by this attempt.
    pub const fn published_cells(self) -> usize {
        self.published_cells
    }

    /// Returns whether the logical message's `LAST` cell was published.
    pub const fn is_complete(self) -> bool {
        self.complete
    }
}

/// Progress made by one nonblocking receive or discard attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcReceiveProgress {
    written: usize,
    consumed_cells: usize,
    complete: bool,
}

impl IvcReceiveProgress {
    pub(crate) const fn new(written: usize, consumed_cells: usize, complete: bool) -> Self {
        Self {
            written,
            consumed_cells,
            complete,
        }
    }

    /// Returns the number of logical payload bytes copied into caller output.
    pub const fn written(self) -> usize {
        self.written
    }

    /// Returns the number of complete cells released to the producer.
    pub const fn consumed_cells(self) -> usize {
        self.consumed_cells
    }

    /// Returns whether `LAST` completed the logical message.
    pub const fn is_complete(self) -> bool {
        self.complete
    }
}
