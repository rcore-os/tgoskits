use core::{fmt, mem::ManuallyDrop, ptr};

use crate::{DEFAULT_BUF_SIZE, Error, IntoInnerError, IoBufMut, Result, Seek, SeekFrom, Write};

#[cfg(feature = "alloc")]
type Buffer = alloc::vec::Vec<u8>;
#[cfg(not(feature = "alloc"))]
type Buffer = heapless::Vec<u8, DEFAULT_BUF_SIZE, u16>;

/// Wraps a writer and buffers its output.
///
/// See [std::io::BufWriter] for more details.
pub struct BufWriter<W: ?Sized + Write> {
    // The buffer.
    buf: Buffer,
    // #30888: If the inner writer panics in a call to write, we don't want to
    // write the buffered data a second time in BufWriter's destructor. This
    // flag tells the Drop impl if it should skip the flush.
    panicked: bool,
    inner: W,
}

impl<W: Write> BufWriter<W> {
    /// Creates a new `BufWriter<W>` with a default buffer capacity.
    pub fn new(inner: W) -> BufWriter<W> {
        #[cfg(feature = "alloc")]
        let buf = Buffer::with_capacity(DEFAULT_BUF_SIZE);
        #[cfg(not(feature = "alloc"))]
        let buf = Buffer::new();
        BufWriter {
            buf,
            panicked: false,
            inner,
        }
    }

    /// Creates a new `BufWriter<W>` with at least the specified buffer capacity.
    #[cfg(feature = "alloc")]
    pub fn with_capacity(capacity: usize, inner: W) -> BufWriter<W> {
        BufWriter {
            buf: Buffer::with_capacity(capacity),
            panicked: false,
            inner,
        }
    }

    /// Unwraps this `BufWriter<W>`, returning the underlying writer.
    ///
    /// The buffer is written out before returning the writer.
    ///
    /// # Errors
    ///
    /// An [`Err`] will be returned if an error occurs while flushing the buffer.
    #[cfg_attr(not(feature = "alloc"), allow(clippy::result_large_err))]
    pub fn into_inner(mut self) -> core::result::Result<W, IntoInnerError<BufWriter<W>>> {
        match self.flush_buf() {
            Err(e) => Err(IntoInnerError::new(self, e)),
            Ok(()) => Ok(self.into_parts().0),
        }
    }

    /// Disassembles this `BufWriter<W>`, returning the underlying writer, and any buffered but
    /// unwritten data.
    ///
    /// If the underlying writer panicked, it is not known what portion of the data was written.
    /// In this case, we return `WriterPanicked` for the buffered data (from which the buffer
    /// contents can still be recovered).
    ///
    /// `into_parts` makes no attempt to flush data and cannot fail.
    pub fn into_parts(self) -> (W, core::result::Result<Buffer, WriterPanicked>) {
        let mut this = ManuallyDrop::new(self);
        let buf = core::mem::take(&mut this.buf);
        let buf = if !this.panicked {
            Ok(buf)
        } else {
            Err(WriterPanicked { buf })
        };

        // SAFETY: double-drops are prevented by putting `this` in a ManuallyDrop that is never
        // dropped
        let inner = unsafe { ptr::read(&this.inner) };

        (inner, buf)
    }
}

/// Error returned for the buffered data from `BufWriter::into_parts`, when the underlying
/// writer has previously panicked.  Contains the (possibly partly written) buffered data.
pub struct WriterPanicked {
    buf: Buffer,
}

impl WriterPanicked {
    /// Returns the perhaps-unwritten data.  Some of this data may have been written by the
    /// panicking call(s) to the underlying writer, so simply writing it again is not a good idea.
    #[must_use = "`self` will be dropped if the result is not used"]
    pub fn into_inner(self) -> Buffer {
        self.buf
    }
}

impl fmt::Display for WriterPanicked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "BufWriter inner writer panicked, what data remains unwritten is not known".fmt(f)
    }
}

impl fmt::Debug for WriterPanicked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriterPanicked")
            .field(
                "buffer",
                &format_args!("{}/{}", self.buf.len(), self.buf.capacity()),
            )
            .finish()
    }
}

impl<W: ?Sized + Write> BufWriter<W> {
    /// Gets a reference to the underlying writer.
    pub fn get_ref(&self) -> &W {
        &self.inner
    }

    /// Gets a mutable reference to the underlying writer.
    ///
    /// It is inadvisable to directly write to the underlying writer.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Returns a reference to the internally buffered data.
    pub fn buffer(&self) -> &[u8] {
        self.buf.as_slice()
    }

    /// Returns the number of bytes the internal buffer can hold without flushing.
    pub fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    pub(crate) fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buf
    }

    /// Send data in our local buffer into the inner writer, looping as
    /// necessary until either it's all been sent or an error occurs.
    ///
    /// Because all the data in the buffer has been reported to our owner as
    /// "successfully written" (by returning nonzero success values from
    /// `write`), any 0-length writes from `inner` must be reported as i/o
    /// errors from this method.
    pub(crate) fn flush_buf(&mut self) -> Result<()> {
        /// Helper struct to ensure the buffer is updated after all the writes
        /// are complete. It tracks the number of written bytes and drains them
        /// all from the front of the buffer when dropped.
        struct BufGuard<'a> {
            buffer: &'a mut Buffer,
            written: usize,
        }

        impl<'a> BufGuard<'a> {
            fn new(buffer: &'a mut Buffer) -> Self {
                Self { buffer, written: 0 }
            }

            /// The unwritten part of the buffer
            fn remaining(&self) -> &[u8] {
                &self.buffer.as_slice()[self.written..]
            }

            /// Flag some bytes as removed from the front of the buffer
            fn consume(&mut self, amt: usize) {
                self.written += amt;
            }

            /// true if all of the bytes have been written
            fn done(&self) -> bool {
                self.written >= self.buffer.len()
            }
        }

        impl Drop for BufGuard<'_> {
            fn drop(&mut self) {
                if self.written > 0 {
                    self.buffer.drain(..self.written);
                }
            }
        }

        let mut guard = BufGuard::new(&mut self.buf);
        while !guard.done() {
            self.panicked = true;
            let r = self.inner.write(guard.remaining());
            self.panicked = false;

            match r {
                Ok(0) => {
                    return Err(Error::WriteZero);
                }
                Ok(n) => guard.consume(n),
                Err(ref e) if e.canonicalize() == Error::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn spare_capacity(&self) -> usize {
        self.buf.capacity() - self.buf.len()
    }

    // SAFETY: Requires `buf.len() <= self.buf.capacity() - self.buf.len()`,
    // i.e., that input buffer length is less than or equal to spare capacity.
    #[inline]
    unsafe fn write_to_buffer_unchecked(&mut self, buf: &[u8]) {
        debug_assert!(buf.len() <= self.spare_capacity());
        let old_len = self.buf.len();
        let buf_len = buf.len();
        let src = buf.as_ptr();
        unsafe {
            let dst = self.buf.as_mut_ptr().add(old_len);
            core::ptr::copy_nonoverlapping(src, dst, buf_len);
            self.buf.set_len(old_len + buf_len);
        }
    }

    /// Buffer some data without flushing it, regardless of the size of the
    /// data. Writes as much as possible without exceeding capacity. Returns
    /// the number of bytes written.
    pub(crate) fn write_to_buf(&mut self, buf: &[u8]) -> usize {
        let available = self.spare_capacity();
        let amt_to_buffer = available.min(buf.len());

        // SAFETY: `amt_to_buffer` is <= buffer's spare capacity by construction.
        unsafe {
            self.write_to_buffer_unchecked(&buf[..amt_to_buffer]);
        }

        amt_to_buffer
    }

    // Ensure this function does not get inlined into `write`, so that it
    // remains inlineable and its common path remains as short as possible.
    // If this function ends up being called frequently relative to `write`,
    // it's likely a sign that the client is using an improperly sized buffer
    // or their write patterns are somewhat pathological.
    #[cold]
    #[inline(never)]
    fn write_cold(&mut self, buf: &[u8]) -> Result<usize> {
        if buf.len() > self.spare_capacity() {
            self.flush_buf()?;
        }

        // Why not len > capacity? To avoid a needless trip through the buffer when the input
        // exactly fills it. We'd just need to flush it to the underlying writer anyway.
        if buf.len() >= self.buf.capacity() {
            self.panicked = true;
            let r = self.get_mut().write(buf);
            self.panicked = false;
            r
        } else {
            // Write to the buffer. In this case, we write to the buffer even if it fills it
            // exactly. Doing otherwise would mean flushing the buffer, then writing this
            // input to the inner writer, which in many cases would be a worse strategy.

            // SAFETY: There was either enough spare capacity already, or there wasn't and we
            // flushed the buffer to ensure that there is. In the latter case, we know that there
            // is because flushing ensured that our entire buffer is spare capacity, and we entered
            // this block because the input buffer length is less than that capacity. In either
            // case, it's safe to write the input buffer to our buffer.
            unsafe {
                self.write_to_buffer_unchecked(buf);
            }

            Ok(buf.len())
        }
    }

    // Ensure this function does not get inlined into `write_all`, so that it
    // remains inlineable and its common path remains as short as possible.
    // If this function ends up being called frequently relative to `write_all`,
    // it's likely a sign that the client is using an improperly sized buffer
    // or their write patterns are somewhat pathological.
    #[cold]
    #[inline(never)]
    fn write_all_cold(&mut self, buf: &[u8]) -> Result<()> {
        // Normally, `write_all` just calls `write` in a loop. We can do better
        // by calling `self.get_mut().write_all()` directly, which avoids
        // round trips through the buffer in the event of a series of partial
        // writes in some circumstances.

        if buf.len() > self.spare_capacity() {
            self.flush_buf()?;
        }

        // Why not len > capacity? To avoid a needless trip through the buffer when the input
        // exactly fills it. We'd just need to flush it to the underlying writer anyway.
        if buf.len() >= self.buf.capacity() {
            self.panicked = true;
            let r = self.get_mut().write_all(buf);
            self.panicked = false;
            r
        } else {
            // Write to the buffer. In this case, we write to the buffer even if it fills it
            // exactly. Doing otherwise would mean flushing the buffer, then writing this
            // input to the inner writer, which in many cases would be a worse strategy.

            // SAFETY: There was either enough spare capacity already, or there wasn't and we
            // flushed the buffer to ensure that there is. In the latter case, we know that there
            // is because flushing ensured that our entire buffer is spare capacity, and we entered
            // this block because the input buffer length is less than that capacity. In either
            // case, it's safe to write the input buffer to our buffer.
            unsafe {
                self.write_to_buffer_unchecked(buf);
            }

            Ok(())
        }
    }
}

impl<W: ?Sized + Write + fmt::Debug> fmt::Debug for BufWriter<W> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("BufWriter")
            .field("writer", &&self.inner)
            .field(
                "buffer",
                &format_args!("{}/{}", self.buf.len(), self.buf.capacity()),
            )
            .finish()
    }
}

impl<W: ?Sized + Write> Write for BufWriter<W> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        // Use < instead of <= to avoid a needless trip through the buffer in some cases.
        // See `write_cold` for details.
        if buf.len() < self.spare_capacity() {
            // SAFETY: safe by above conditional.
            unsafe {
                self.write_to_buffer_unchecked(buf);
            }

            Ok(buf.len())
        } else {
            self.write_cold(buf)
        }
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        // Use < instead of <= to avoid a needless trip through the buffer in some cases.
        // See `write_all_cold` for details.
        if buf.len() < self.spare_capacity() {
            // SAFETY: safe by above conditional.
            unsafe {
                self.write_to_buffer_unchecked(buf);
            }

            Ok(())
        } else {
            self.write_all_cold(buf)
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.flush_buf().and_then(|()| self.get_mut().flush())
    }
}

impl<W: ?Sized + Write + Seek> Seek for BufWriter<W> {
    /// Seek to the offset, in bytes, in the underlying writer.
    ///
    /// Seeking always writes out the internal buffer before seeking.
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        self.flush_buf()?;
        self.get_mut().seek(pos)
    }
}

impl<W: ?Sized + Write> Drop for BufWriter<W> {
    fn drop(&mut self) {
        if !self.panicked {
            // dtors should not panic, so we ignore a failed flush
            let _r = self.flush_buf();
        }
    }
}

impl<W: ?Sized + Write + IoBufMut> IoBufMut for BufWriter<W> {
    #[inline]
    fn remaining_mut(&self) -> usize {
        self.inner.remaining_mut().saturating_sub(self.buf.len())
    }
}
