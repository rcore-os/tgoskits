mod buffer;

#[cfg(feature = "alloc")]
use alloc::{string::String, vec::Vec};
use core::{fmt, io::BorrowedCursor};

use self::buffer::Buffer;
#[cfg(feature = "alloc")]
use crate::Error;
use crate::{BufRead, DEFAULT_BUF_SIZE, IoBuf, Read, Result, Seek, SeekFrom};

/// The `BufReader<R>` struct adds buffering to any reader.
///
/// See [`std::io::BufReader`] for more details.
pub struct BufReader<R: ?Sized> {
    buf: Buffer,
    inner: R,
}

impl<R: Read> BufReader<R> {
    /// Creates a new `BufReader<R>` with the specified buffer capacity.
    pub fn with_capacity(capacity: usize, inner: R) -> BufReader<R> {
        BufReader {
            buf: Buffer::with_capacity(capacity),
            inner,
        }
    }

    /// Creates a new `BufReader<R>` with a default buffer capacity.
    pub fn new(inner: R) -> BufReader<R> {
        BufReader::with_capacity(DEFAULT_BUF_SIZE, inner)
    }
}

impl<R: ?Sized> BufReader<R> {
    /// Gets a reference to the underlying reader.
    ///
    /// It is inadvisable to directly read from the underlying reader.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Gets a mutable reference to the underlying reader.
    ///
    /// It is inadvisable to directly read from the underlying reader.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Returns a reference to the internally buffered data.
    ///
    /// Unlike [`fill_buf`], this will not attempt to fill the buffer if it is empty.
    ///
    /// [`fill_buf`]: BufRead::fill_buf
    pub fn buffer(&self) -> &[u8] {
        self.buf.buffer()
    }

    /// Returns the number of bytes the internal buffer can hold at once.
    pub fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    #[cfg(borrowedbuf_init)]
    #[doc(hidden)]
    pub fn initialized(&self) -> usize {
        self.buf.initialized()
    }

    /// Unwraps this `BufReader<R>`, returning the underlying reader.
    ///
    /// Note that any leftover data in the internal buffer is lost. Therefore,
    /// a following read from the underlying reader may lead to data loss.
    pub fn into_inner(self) -> R
    where
        R: Sized,
    {
        self.inner
    }

    /// Invalidates all data in the internal buffer.
    #[inline]
    pub(crate) fn discard_buffer(&mut self) {
        self.buf.discard_buffer()
    }

    #[inline]
    pub(crate) fn consume(&mut self, amt: usize) {
        self.buf.consume(amt)
    }
}

impl<R: Read + ?Sized> BufReader<R> {
    /// Attempt to look ahead `n` bytes.
    ///
    /// `n` must be less than or equal to `capacity`.
    ///
    /// The returned slice may be less than `n` bytes long if
    /// end of file is reached.
    ///
    /// After calling this method, you may call [`consume`](BufRead::consume)
    /// with a value less than or equal to `n` to advance over some or all of
    /// the returned bytes.
    pub fn peek(&mut self, n: usize) -> Result<&[u8]> {
        assert!(n <= self.capacity());
        while n > self.buf.buffer().len() {
            if self.buf.pos() > 0 {
                self.buf.backshift();
            }
            let new = self.buf.read_more(&mut self.inner)?;
            if new == 0 {
                // end of file, no more bytes to read
                return Ok(self.buf.buffer());
            }
            debug_assert_eq!(self.buf.pos(), 0);
        }
        Ok(&self.buf.buffer()[..n])
    }
}

impl<R> fmt::Debug for BufReader<R>
where
    R: ?Sized + fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("BufReader")
            .field("reader", &&self.inner)
            .field(
                "buffer",
                &format_args!("{}/{}", self.buf.filled() - self.buf.pos(), self.capacity()),
            )
            .finish()
    }
}

impl<R: ?Sized + Read> Read for BufReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        // If we don't have any buffered data and we're doing a massive read
        // (larger than our internal buffer), bypass our internal buffer
        // entirely.
        if self.buf.pos() == self.buf.filled() && buf.len() >= self.capacity() {
            self.discard_buffer();
            return self.inner.read(buf);
        }
        let mut rem = self.fill_buf()?;
        let nread = rem.read(buf)?;
        self.consume(nread);
        Ok(nread)
    }

    fn read_buf(&mut self, mut cursor: BorrowedCursor<'_>) -> Result<()> {
        // If we don't have any buffered data and we're doing a massive read
        // (larger than our internal buffer), bypass our internal buffer
        // entirely.
        if self.buf.pos() == self.buf.filled() && cursor.capacity() >= self.capacity() {
            self.discard_buffer();
            return self.inner.read_buf(cursor);
        }

        let prev = cursor.written();

        let mut rem = self.fill_buf()?;
        rem.read_buf(cursor.reborrow())?; // actually never fails

        self.consume(cursor.written() - prev); // slice impl of read_buf known to never unfill buf

        Ok(())
    }

    // Small read_exacts from a BufReader are extremely common when used with a deserializer.
    // The default implementation calls read in a loop, which results in surprisingly poor code
    // generation for the common path where the buffer has enough bytes to fill the passed-in
    // buffer.
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        if self
            .buf
            .consume_with(buf.len(), |claimed| buf.copy_from_slice(claimed))
        {
            return Ok(());
        }

        crate::default_read_exact(self, buf)
    }

    fn read_buf_exact(&mut self, mut cursor: BorrowedCursor<'_>) -> Result<()> {
        if self
            .buf
            .consume_with(cursor.capacity(), |claimed| cursor.append(claimed))
        {
            return Ok(());
        }

        crate::default_read_buf_exact(self, cursor)
    }

    // The inner reader might have an optimized `read_to_end`. Drain our buffer and then
    // delegate to the inner implementation.
    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        let inner_buf = self.buffer();
        buf.try_reserve(inner_buf.len())
            .map_err(|_| Error::NoMemory)?;
        buf.extend_from_slice(inner_buf);
        let nread = inner_buf.len();
        self.discard_buffer();
        Ok(nread + self.inner.read_to_end(buf)?)
    }

    // The inner reader might have an optimized `read_to_end`. Drain our buffer and then
    // delegate to the inner implementation.
    #[cfg(feature = "alloc")]
    fn read_to_string(&mut self, buf: &mut String) -> Result<usize> {
        // In the general `else` case below we must read bytes into a side buffer, check
        // that they are valid UTF-8, and then append them to `buf`. This requires a
        // potentially large memcpy.
        //
        // If `buf` is empty--the most common case--we can leverage `append_to_string`
        // to read directly into `buf`'s internal byte buffer, saving an allocation and
        // a memcpy.

        if buf.is_empty() {
            // `append_to_string`'s safety relies on the buffer only being appended to since
            // it only checks the UTF-8 validity of new data. If there were existing content in
            // `buf` then an untrustworthy reader (i.e. `self.inner`) could not only append
            // bytes but also modify existing bytes and render them invalid. On the other hand,
            // if `buf` is empty then by definition any writes must be appends and
            // `append_to_string` will validate all of the new bytes.
            unsafe { crate::append_to_string(buf, |b| self.read_to_end(b)) }
        } else {
            // We cannot append our byte buffer directly onto the `buf` String as there could
            // be an incomplete UTF-8 sequence that has only been partially read. We must read
            // everything into a side buffer first and then call `from_utf8` on the complete
            // buffer.
            let mut bytes = Vec::new();
            self.read_to_end(&mut bytes)?;
            let string = str::from_utf8(&bytes).map_err(|_| Error::IllegalBytes)?;
            *buf += string;
            Ok(string.len())
        }
    }
}

impl<R: ?Sized + Read> BufRead for BufReader<R> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        self.buf.fill_buf(&mut self.inner)
    }

    fn consume(&mut self, amt: usize) {
        self.buf.consume(amt)
    }
}

impl<R: ?Sized + Seek> Seek for BufReader<R> {
    /// Seek to an offset, in bytes, in the underlying reader.
    ///
    /// The position used for seeking with <code>[SeekFrom::Current]\(_)</code> is the
    /// position the underlying reader would be at if the `BufReader<R>` had no
    /// internal buffer.
    ///
    /// Seeking always discards the internal buffer, even if the seek position
    /// would otherwise fall within it. This guarantees that calling
    /// [`BufReader::into_inner()`] immediately after a seek yields the underlying reader
    /// at the same position.
    ///
    /// To seek without discarding the internal buffer, use [`BufReader::seek_relative`].
    ///
    /// See [`Seek`] for more details.
    ///
    /// Note: In the edge case where you're seeking with <code>[SeekFrom::Current]\(n)</code>
    /// where `n` minus the internal buffer length overflows an `i64`, two
    /// seeks will be performed instead of one. If the second seek returns
    /// [`Err`], the underlying reader will be left at the same position it would
    /// have if you called `seek` with <code>[SeekFrom::Current]\(0)</code>.
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        let result: u64;
        if let SeekFrom::Current(n) = pos {
            let remainder = (self.buf.filled() - self.buf.pos()) as i64;
            // it should be safe to assume that remainder fits within an i64 as the alternative
            // means we managed to allocate 8 exbibytes and that's absurd.
            // But it's not out of the realm of possibility for some weird underlying reader to
            // support seeking by i64::MIN so we need to handle underflow when subtracting
            // remainder.
            if let Some(offset) = n.checked_sub(remainder) {
                result = self.inner.seek(SeekFrom::Current(offset))?;
            } else {
                // seek backwards by our remainder, and then by the offset
                self.inner.seek(SeekFrom::Current(-remainder))?;
                self.discard_buffer();
                result = self.inner.seek(SeekFrom::Current(n))?;
            }
        } else {
            // Seeking with Start/End doesn't care about our buffer length.
            result = self.inner.seek(pos)?;
        }
        self.discard_buffer();
        Ok(result)
    }

    /// Returns the current seek position from the start of the stream.
    ///
    /// The value returned is equivalent to `self.seek(SeekFrom::Current(0))`
    /// but does not flush the internal buffer. Due to this optimization the
    /// function does not guarantee that calling `.into_inner()` immediately
    /// afterwards will yield the underlying reader at the same position. Use
    /// [`BufReader::seek`] instead if you require that guarantee.
    ///
    /// # Panics
    ///
    /// This function will panic if the position of the inner reader is smaller
    /// than the amount of buffered data. That can happen if the inner reader
    /// has an incorrect implementation of [`Seek::stream_position`], or if the
    /// position has gone out of sync due to calling [`Seek::seek`] directly on
    /// the underlying reader.
    fn stream_position(&mut self) -> Result<u64> {
        let remainder = (self.buf.filled() - self.buf.pos()) as u64;
        self.inner.stream_position().map(|pos| {
            pos.checked_sub(remainder).expect(
                "overflow when subtracting remaining buffer size from inner stream position",
            )
        })
    }

    /// Seeks relative to the current position.
    ///
    /// If the new position lies within the buffer, the buffer will not be
    /// flushed, allowing for more efficient seeks. This method does not return
    /// the location of the underlying reader, so the caller must track this
    /// information themselves if it is required.
    fn seek_relative(&mut self, offset: i64) -> Result<()> {
        let pos = self.buf.pos() as u64;
        if offset < 0 {
            if pos.checked_sub((-offset) as u64).is_some() {
                self.buf.unconsume((-offset) as usize);
                return Ok(());
            }
        } else if let Some(new_pos) = pos.checked_add(offset as u64)
            && new_pos <= self.buf.filled() as u64
        {
            self.buf.consume(offset as usize);
            return Ok(());
        }

        self.seek(SeekFrom::Current(offset)).map(drop)
    }
}

impl<R: ?Sized + IoBuf> IoBuf for BufReader<R> {
    #[inline]
    fn remaining(&self) -> usize {
        self.inner.remaining() + self.buf.filled() - self.buf.pos()
    }
}
