#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::io::BorrowedCursor;

use crate::{BufRead, IoBuf, Read, Result};

/// Adapter to chain together two readers.
///
/// This struct is generally created by calling [`chain`] on a reader.
/// Please see the documentation of [`chain`] for more details.
///
/// See [`std::io::Chain`] for more details.
///
/// [`chain`]: Read::chain
#[derive(Debug)]
pub struct Chain<T, U> {
    first: T,
    second: U,
    done_first: bool,
}

impl<T, U> Chain<T, U> {
    pub(crate) fn new(first: T, second: U) -> Self {
        Chain {
            first,
            second,
            done_first: false,
        }
    }

    /// Consumes the `Chain`, returning the wrapped readers.
    pub fn into_inner(self) -> (T, U) {
        (self.first, self.second)
    }

    /// Gets references to the underlying readers in this `Chain`.
    ///
    /// Care should be taken to avoid modifying the internal I/O state of the
    /// underlying readers as doing so may corrupt the internal state of this
    /// `Chain`.
    pub fn get_ref(&self) -> (&T, &U) {
        (&self.first, &self.second)
    }

    /// Gets mutable references to the underlying readers in this `Chain`.
    ///
    /// Care should be taken to avoid modifying the internal I/O state of the
    /// underlying readers as doing so may corrupt the internal state of this
    /// `Chain`.
    pub fn get_mut(&mut self) -> (&mut T, &mut U) {
        (&mut self.first, &mut self.second)
    }
}

impl<T: Read, U: Read> Read for Chain<T, U> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if !self.done_first {
            match self.first.read(buf)? {
                0 if !buf.is_empty() => self.done_first = true,
                n => return Ok(n),
            }
        }
        self.second.read(buf)
    }

    fn read_buf(&mut self, mut buf: BorrowedCursor<'_>) -> Result<()> {
        if buf.capacity() == 0 {
            return Ok(());
        }

        if !self.done_first {
            let old_len = buf.written();
            self.first.read_buf(buf.reborrow())?;

            if buf.written() != old_len {
                return Ok(());
            } else {
                self.done_first = true;
            }
        }
        self.second.read_buf(buf)
    }

    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        let mut read = 0;
        if !self.done_first {
            read += self.first.read_to_end(buf)?;
            self.done_first = true;
        }
        read += self.second.read_to_end(buf)?;
        Ok(read)
    }

    // We don't override `read_to_string` here because an UTF-8 sequence could
    // be split between the two parts of the chain
}

impl<T: BufRead, U: BufRead> BufRead for Chain<T, U> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        if !self.done_first {
            match self.first.fill_buf()? {
                [] => self.done_first = true,
                buf => return Ok(buf),
            }
        }
        self.second.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        if !self.done_first {
            self.first.consume(amt)
        } else {
            self.second.consume(amt)
        }
    }

    #[cfg(feature = "alloc")]
    fn read_until(&mut self, byte: u8, buf: &mut Vec<u8>) -> Result<usize> {
        let mut read = 0;
        if !self.done_first {
            let n = self.first.read_until(byte, buf)?;
            read += n;

            match buf.last() {
                Some(b) if *b == byte && n != 0 => return Ok(read),
                _ => self.done_first = true,
            }
        }
        read += self.second.read_until(byte, buf)?;
        Ok(read)
    }

    // We don't override `read_line` here because an UTF-8 sequence could be
    // split between the two parts of the chain
}

impl<T: IoBuf, U: IoBuf> IoBuf for Chain<T, U> {
    #[inline]
    fn remaining(&self) -> usize {
        self.first.remaining() + self.second.remaining()
    }
}
