#[cfg(feature = "alloc")]
use alloc::{string::String, vec::Vec};
use core::{fmt, io::BorrowedCursor};

use crate::{IoBuf, Read, Result};

/// A reader which yields one byte over and over and over and over and over and...
///
/// This struct is generally created by calling [`repeat()`]. Please
/// see the documentation of [`repeat()`] for more details.
pub struct Repeat {
    byte: u8,
}

/// Creates an instance of a reader that infinitely repeats one byte.
///
/// All reads from this reader will succeed by filling the specified buffer with
/// the given byte.
///
/// See [`std::io::repeat()`] for more details.
#[must_use]
pub const fn repeat(byte: u8) -> Repeat {
    Repeat { byte }
}

impl Read for Repeat {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        buf.fill(self.byte);
        Ok(buf.len())
    }

    #[inline]
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        buf.fill(self.byte);
        Ok(())
    }

    #[inline]
    fn read_buf(&mut self, mut buf: BorrowedCursor<'_>) -> Result<()> {
        // SAFETY: No uninit bytes are being written.
        unsafe { buf.as_mut() }.write_filled(self.byte);
        // SAFETY: the entire unfilled portion of buf has been initialized.
        unsafe {
            #[cfg(borrowedbuf_init)]
            buf.advance_unchecked(buf.capacity());
            #[cfg(not(borrowedbuf_init))]
            buf.advance(buf.capacity());
        };
        Ok(())
    }

    #[inline]
    fn read_buf_exact(&mut self, buf: BorrowedCursor<'_>) -> Result<()> {
        self.read_buf(buf)
    }

    /// This function is not supported by `Repeat`, because there's no end of its data
    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, _: &mut Vec<u8>) -> Result<usize> {
        Err(crate::Error::NoMemory)
    }

    /// This function is not supported by `Repeat`, because there's no end of its data
    #[cfg(feature = "alloc")]
    fn read_to_string(&mut self, _: &mut String) -> Result<usize> {
        Err(crate::Error::NoMemory)
    }
}

impl fmt::Debug for Repeat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Repeat").finish_non_exhaustive()
    }
}

impl IoBuf for Repeat {
    fn remaining(&self) -> usize {
        usize::MAX
    }
}
