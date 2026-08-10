/// Message class used by the fixed-slot IVC protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum IvcMessageKind {
    /// Publisher request payload.
    Request = 1,
    /// Subscriber acknowledgement payload.
    Ack     = 2,
}

impl IvcMessageKind {
    pub(crate) const fn from_raw(raw: u16) -> Option<Self> {
        match raw {
            1 => Some(Self::Request),
            2 => Some(Self::Ack),
            _ => None,
        }
    }
}

/// One message copied out of an IVC ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcMessage {
    sequence: u64,
    kind: IvcMessageKind,
    len: usize,
}

impl IvcMessage {
    pub(crate) const fn new(sequence: u64, kind: IvcMessageKind, len: usize) -> Self {
        Self {
            sequence,
            kind,
            len,
        }
    }

    /// Returns the message sequence number.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Returns the message kind.
    pub const fn kind(self) -> IvcMessageKind {
        self.kind
    }

    /// Returns the copied payload length.
    pub const fn len(self) -> usize {
        self.len
    }

    /// Returns whether the copied payload is empty.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}
