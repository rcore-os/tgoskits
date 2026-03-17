use crate::Result;

mod impls;

/// Enumeration of possible methods to seek within an I/O object.
///
/// It is used by the [`Seek`] trait.
#[derive(Copy, PartialEq, Eq, Clone, Debug)]
pub enum SeekFrom {
    /// Sets the offset to the provided number of bytes.
    Start(u64),

    /// Sets the offset to the size of this object plus the specified number of
    /// bytes.
    ///
    /// It is possible to seek beyond the end of an object, but it's an error to
    /// seek before byte 0.
    End(i64),

    /// Sets the offset to the current position plus the specified number of
    /// bytes.
    ///
    /// It is possible to seek beyond the end of an object, but it's an error to
    /// seek before byte 0.
    Current(i64),
}

/// Default [`Seek::stream_len`] implementation.
pub fn default_stream_len<T: Seek + ?Sized>(this: &mut T) -> Result<u64> {
    let old_pos = this.stream_position()?;
    let len = this.seek(SeekFrom::End(0))?;

    // Avoid seeking a third time when we were already at the end of the
    // stream. The branch is usually way cheaper than a seek operation.
    if old_pos != len {
        this.seek(SeekFrom::Start(old_pos))?;
    }

    Ok(len)
}

/// The `Seek` trait provides a cursor which can be moved within a stream of
/// bytes.
///
/// See [`std::io::Seek`] for more details.
pub trait Seek {
    /// Seek to an offset, in bytes, in a stream.
    ///
    /// A seek beyond the end of a stream is allowed, but behavior is defined
    /// by the implementation.
    ///
    /// If the seek operation completed successfully,
    /// this method returns the new position from the start of the stream.
    /// That position can be used later with [`SeekFrom::Start`].
    fn seek(&mut self, pos: SeekFrom) -> Result<u64>;

    /// Rewind to the beginning of a stream.
    ///
    /// This is a convenience method, equivalent to `seek(SeekFrom::Start(0))`.
    fn rewind(&mut self) -> Result<()> {
        self.seek(SeekFrom::Start(0))?;
        Ok(())
    }

    /// Returns the current seek position from the start of the stream.
    ///
    /// This is equivalent to `self.seek(SeekFrom::Current(0))`.
    fn stream_position(&mut self) -> Result<u64> {
        self.seek(SeekFrom::Current(0))
    }

    /// Returns the length of this stream (in bytes).
    fn stream_len(&mut self) -> Result<u64> {
        default_stream_len(self)
    }

    /// Seeks relative to the current position.
    fn seek_relative(&mut self, offset: i64) -> Result<()> {
        self.seek(SeekFrom::Current(offset))?;
        Ok(())
    }
}
