mod ext;
mod impls;

pub use self::ext::{IoBufExt, IoBufMutExt};

/// A trait for byte buffers that can be used as a source of bytes to read.
///
/// This is an optional extension to [`Read`](crate::Read). A reader may not have a deterministic
/// length, but an `IoBuf` can report how many bytes are remaining to be read.
pub trait IoBuf {
    /// Returns the number of bytes between the current position and the end of the buffer.
    fn remaining(&self) -> usize;

    /// Returns `true` if there are no remaining bytes in the buffer.
    #[inline]
    fn is_empty(&self) -> bool {
        self.remaining() == 0
    }
}

/// A trait for byte buffers that can be used as a destination for bytes to write.
///
/// This is an optional extension to [`Write`](crate::Write). A writer may not have a deterministic
/// length, but an `IoBufMut` can report how much space is remaining to be written.
pub trait IoBufMut {
    /// Returns the number of bytes that can be written from the current position until the end of
    /// the buffer is reached.
    fn remaining_mut(&self) -> usize;

    /// Returns `true` if there is no remaining space in the buffer.
    #[inline]
    fn is_full(&self) -> bool {
        self.remaining_mut() == 0
    }
}
