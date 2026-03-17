#[cfg(feature = "alloc")]
use alloc::{string::String, vec::Vec};
use core::{fmt, io::BorrowedCursor};

use crate::{BufRead, Error, IoBuf, IoBufMut, Read, Result, Seek, SeekFrom, Write};

/// `Empty` ignores any data written via [`Write`], and will always be empty
/// (returning zero bytes) when read via [`Read`].
///
/// This struct is generally created by calling [`empty()`]. Please
/// see the documentation of [`empty()`] for more details.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Default)]
pub struct Empty;

/// Creates a value that is always at EOF for reads, and ignores all data written.
///
/// See [`std::io::empty()`] for more details.
#[must_use]
pub const fn empty() -> Empty {
    Empty
}

impl Read for Empty {
    #[inline]
    fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    #[inline]
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        if !buf.is_empty() {
            Err(Error::UnexpectedEof)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn read_buf(&mut self, _cursor: BorrowedCursor<'_>) -> Result<()> {
        Ok(())
    }

    #[inline]
    fn read_buf_exact(&mut self, cursor: BorrowedCursor<'_>) -> Result<()> {
        if cursor.capacity() != 0 {
            Err(Error::UnexpectedEof)
        } else {
            Ok(())
        }
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, _buf: &mut Vec<u8>) -> Result<usize> {
        Ok(0)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_string(&mut self, _buf: &mut String) -> Result<usize> {
        Ok(0)
    }
}

impl BufRead for Empty {
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(&[])
    }

    #[inline]
    fn consume(&mut self, _n: usize) {}

    #[inline]
    fn has_data_left(&mut self) -> Result<bool> {
        Ok(false)
    }

    #[inline]
    fn skip_until(&mut self, _byte: u8) -> Result<usize> {
        Ok(0)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_until(&mut self, _byte: u8, _buf: &mut Vec<u8>) -> Result<usize> {
        Ok(0)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_line(&mut self, _buf: &mut String) -> Result<usize> {
        Ok(0)
    }
}

impl Seek for Empty {
    #[inline]
    fn seek(&mut self, _pos: SeekFrom) -> Result<u64> {
        Ok(0)
    }

    #[inline]
    fn stream_len(&mut self) -> Result<u64> {
        Ok(0)
    }

    #[inline]
    fn stream_position(&mut self) -> Result<u64> {
        Ok(0)
    }
}

impl Write for Empty {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        Ok(buf.len())
    }

    #[inline]
    fn write_all(&mut self, _buf: &[u8]) -> Result<()> {
        Ok(())
    }

    #[inline]
    fn write_fmt(&mut self, _args: fmt::Arguments<'_>) -> Result<()> {
        Ok(())
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl Write for &Empty {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        Ok(buf.len())
    }

    #[inline]
    fn write_all(&mut self, _buf: &[u8]) -> Result<()> {
        Ok(())
    }

    #[inline]
    fn write_fmt(&mut self, _args: fmt::Arguments<'_>) -> Result<()> {
        Ok(())
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl IoBuf for Empty {
    #[inline]
    fn remaining(&self) -> usize {
        0
    }
}

impl IoBufMut for Empty {
    #[inline]
    fn remaining_mut(&self) -> usize {
        usize::MAX
    }
}
