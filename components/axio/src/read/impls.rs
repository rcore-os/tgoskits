#[cfg(feature = "alloc")]
use alloc::{boxed::Box, collections::VecDeque, string::String, vec::Vec};
use core::{cmp, io::BorrowedCursor};

use crate::{BufRead, Error, Read, Result};

// =============================================================================
// Forwarding implementations

impl<R: Read + ?Sized> Read for &mut R {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        (**self).read(buf)
    }

    #[inline]
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        (**self).read_exact(buf)
    }

    #[inline]
    fn read_buf(&mut self, cursor: BorrowedCursor<'_>) -> Result<()> {
        (**self).read_buf(cursor)
    }

    #[inline]
    fn read_buf_exact(&mut self, cursor: BorrowedCursor<'_>) -> Result<()> {
        (**self).read_buf_exact(cursor)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        (**self).read_to_end(buf)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_string(&mut self, buf: &mut String) -> Result<usize> {
        (**self).read_to_string(buf)
    }
}

impl<B: BufRead + ?Sized> BufRead for &mut B {
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8]> {
        (**self).fill_buf()
    }

    #[inline]
    fn consume(&mut self, amt: usize) {
        (**self).consume(amt)
    }

    #[inline]
    fn has_data_left(&mut self) -> Result<bool> {
        (**self).has_data_left()
    }

    #[inline]
    fn skip_until(&mut self, byte: u8) -> Result<usize> {
        (**self).skip_until(byte)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_until(&mut self, byte: u8, buf: &mut Vec<u8>) -> Result<usize> {
        (**self).read_until(byte, buf)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_line(&mut self, buf: &mut String) -> Result<usize> {
        (**self).read_line(buf)
    }
}

#[cfg(feature = "alloc")]
impl<R: Read + ?Sized> Read for Box<R> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        (**self).read(buf)
    }

    #[inline]
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        (**self).read_exact(buf)
    }

    #[inline]
    fn read_buf(&mut self, cursor: BorrowedCursor<'_>) -> Result<()> {
        (**self).read_buf(cursor)
    }

    #[inline]
    fn read_buf_exact(&mut self, cursor: BorrowedCursor<'_>) -> Result<()> {
        (**self).read_buf_exact(cursor)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        (**self).read_to_end(buf)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_string(&mut self, buf: &mut String) -> Result<usize> {
        (**self).read_to_string(buf)
    }
}

#[cfg(feature = "alloc")]
impl<B: BufRead + ?Sized> BufRead for Box<B> {
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8]> {
        (**self).fill_buf()
    }

    #[inline]
    fn consume(&mut self, amt: usize) {
        (**self).consume(amt)
    }

    #[inline]
    fn has_data_left(&mut self) -> Result<bool> {
        (**self).has_data_left()
    }

    #[inline]
    fn skip_until(&mut self, byte: u8) -> Result<usize> {
        (**self).skip_until(byte)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_until(&mut self, byte: u8, buf: &mut Vec<u8>) -> Result<usize> {
        (**self).read_until(byte, buf)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_line(&mut self, buf: &mut String) -> Result<usize> {
        (**self).read_line(buf)
    }
}

// =============================================================================
// In-memory buffer implementations

impl Read for &[u8] {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let amt = cmp::min(buf.len(), self.len());
        let (a, b) = self.split_at(amt);

        // First check if the amount of bytes we want to read is small:
        // `copy_from_slice` will generally expand to a call to `memcpy`, and
        // for a single byte the overhead is significant.
        if amt == 1 {
            buf[0] = a[0];
        } else {
            buf[..amt].copy_from_slice(a);
        }

        *self = b;
        Ok(amt)
    }

    #[inline]
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        if buf.len() > self.len() {
            // `read_exact` makes no promise about the content of `buf` if it
            // fails so don't bother about that.
            *self = &self[self.len()..];
            return Err(Error::UnexpectedEof);
        }
        let (a, b) = self.split_at(buf.len());

        // First check if the amount of bytes we want to read is small:
        // `copy_from_slice` will generally expand to a call to `memcpy`, and
        // for a single byte the overhead is significant.
        if buf.len() == 1 {
            buf[0] = a[0];
        } else {
            buf.copy_from_slice(a);
        }

        *self = b;
        Ok(())
    }

    #[inline]
    fn read_buf(&mut self, mut cursor: BorrowedCursor<'_>) -> Result<()> {
        let amt = cmp::min(cursor.capacity(), self.len());
        let (a, b) = self.split_at(amt);

        cursor.append(a);

        *self = b;
        Ok(())
    }

    #[inline]
    fn read_buf_exact(&mut self, mut cursor: BorrowedCursor<'_>) -> Result<()> {
        if cursor.capacity() > self.len() {
            // Append everything we can to the cursor.
            cursor.append(self);
            *self = &self[self.len()..];
            return Err(Error::UnexpectedEof);
        }
        let (a, b) = self.split_at(cursor.capacity());

        cursor.append(a);

        *self = b;
        Ok(())
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        let len = self.len();
        buf.try_reserve(len).map_err(|_| Error::NoMemory)?;
        buf.extend_from_slice(self);
        *self = &self[len..];
        Ok(len)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_string(&mut self, buf: &mut String) -> Result<usize> {
        let content = str::from_utf8(self).map_err(|_| Error::IllegalBytes)?;
        let len = self.len();
        buf.try_reserve(len).map_err(|_| Error::NoMemory)?;
        buf.push_str(content);
        *self = &self[len..];
        Ok(len)
    }
}

impl BufRead for &[u8] {
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(*self)
    }

    #[inline]
    fn consume(&mut self, amt: usize) {
        *self = &self[amt..];
    }
}

#[cfg(feature = "alloc")]
impl Read for VecDeque<u8> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let (ref mut front, _) = self.as_slices();
        let n = Read::read(front, buf)?;
        self.drain(..n);
        Ok(n)
    }

    #[inline]
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        let (front, back) = self.as_slices();

        // Use only the front buffer if it is big enough to fill `buf`, else use
        // the back buffer too.
        match buf.split_at_mut_checked(front.len()) {
            None => buf.copy_from_slice(&front[..buf.len()]),
            Some((buf_front, buf_back)) => match back.split_at_checked(buf_back.len()) {
                Some((back, _)) => {
                    buf_front.copy_from_slice(front);
                    buf_back.copy_from_slice(back);
                }
                None => {
                    self.clear();
                    return Err(Error::UnexpectedEof);
                }
            },
        }

        self.drain(..buf.len());
        Ok(())
    }

    #[inline]
    fn read_buf(&mut self, cursor: BorrowedCursor<'_>) -> Result<()> {
        let (ref mut front, _) = self.as_slices();
        let n = cmp::min(cursor.capacity(), front.len());
        Read::read_buf(front, cursor)?;
        self.drain(..n);
        Ok(())
    }

    #[inline]
    fn read_buf_exact(&mut self, mut cursor: BorrowedCursor<'_>) -> Result<()> {
        let len = cursor.capacity();
        let (front, back) = self.as_slices();

        match front.split_at_checked(cursor.capacity()) {
            Some((front, _)) => cursor.append(front),
            None => {
                cursor.append(front);
                match back.split_at_checked(cursor.capacity()) {
                    Some((back, _)) => cursor.append(back),
                    None => {
                        cursor.append(back);
                        self.clear();
                        return Err(Error::UnexpectedEof);
                    }
                }
            }
        }

        self.drain(..len);
        Ok(())
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        // The total len is known upfront so we can reserve it in a single call.
        let len = self.len();
        buf.try_reserve(len).map_err(|_| Error::NoMemory)?;
        let (front, back) = self.as_slices();
        buf.extend_from_slice(front);
        buf.extend_from_slice(back);
        self.clear();
        Ok(len)
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_string(&mut self, buf: &mut String) -> Result<usize> {
        // SAFETY: We only append to the buffer
        unsafe { super::append_to_string(buf, |buf| self.read_to_end(buf)) }
    }
}

#[cfg(feature = "alloc")]
impl BufRead for VecDeque<u8> {
    /// Returns the contents of the "front" slice as returned by
    /// [`as_slices`][`VecDeque::as_slices`]. If the contained byte slices of the `VecDeque` are
    /// discontiguous, multiple calls to `fill_buf` will be needed to read the entire content.
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8]> {
        let (front, _) = self.as_slices();
        Ok(front)
    }

    #[inline]
    fn consume(&mut self, amt: usize) {
        self.drain(..amt);
    }
}
