mod shim;

use core::fmt;

use self::shim::LineWriterShim;
use crate::{BufWriter, IntoInnerError, Result, Write};

/// Wraps a writer and buffers output to it, flushing whenever a newline
/// (`0x0a`, `'\n'`) is detected.
///
/// The [`BufWriter`] struct wraps a writer and buffers its output.
/// But it only does this batched write when it goes out of scope, or when the
/// internal buffer is full. Sometimes, you'd prefer to write each line as it's
/// completed, rather than the entire buffer at once. Enter `LineWriter`. It
/// does exactly that.
///
/// Like [`BufWriter`], a `LineWriter`’s buffer will also be flushed when the
/// `LineWriter` goes out of scope or when its internal buffer is full.
///
/// If there's still a partial line in the buffer when the `LineWriter` is
/// dropped, it will flush those contents.
///
/// See [`std::io::LineWriter`] for more details.
pub struct LineWriter<W: ?Sized + Write> {
    inner: BufWriter<W>,
}

impl<W: Write> LineWriter<W> {
    /// Creates a new `LineWriter`.
    pub fn new(inner: W) -> LineWriter<W> {
        LineWriter {
            inner: BufWriter::new(inner),
        }
    }

    /// Creates a new `LineWriter` with at least the specified capacity for the
    /// internal buffer.
    #[cfg(feature = "alloc")]
    pub fn with_capacity(capacity: usize, inner: W) -> LineWriter<W> {
        LineWriter {
            inner: BufWriter::with_capacity(capacity, inner),
        }
    }

    /// Unwraps this `LineWriter`, returning the underlying writer.
    ///
    /// The internal buffer is written out before returning the writer.
    ///
    /// # Errors
    ///
    /// An [`Err`] will be returned if an error occurs while flushing the buffer.
    #[cfg_attr(not(feature = "alloc"), allow(clippy::result_large_err))]
    pub fn into_inner(self) -> core::result::Result<W, IntoInnerError<LineWriter<W>>> {
        self.inner
            .into_inner()
            .map_err(|err| err.new_wrapped(|inner| LineWriter { inner }))
    }
}

impl<W: ?Sized + Write> LineWriter<W> {
    /// Gets a reference to the underlying writer.
    pub fn get_ref(&self) -> &W {
        self.inner.get_ref()
    }

    /// Gets a mutable reference to the underlying writer.
    ///
    /// Caution must be taken when calling methods on the mutable reference
    /// returned as extra writes could corrupt the output stream.
    pub fn get_mut(&mut self) -> &mut W {
        self.inner.get_mut()
    }
}

impl<W: ?Sized + Write> Write for LineWriter<W> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        LineWriterShim::new(&mut self.inner).write(buf)
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        LineWriterShim::new(&mut self.inner).write_all(buf)
    }

    fn write_fmt(&mut self, fmt: fmt::Arguments<'_>) -> Result<()> {
        LineWriterShim::new(&mut self.inner).write_fmt(fmt)
    }
}

impl<W: ?Sized + Write + fmt::Debug> fmt::Debug for LineWriter<W> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("LineWriter")
            .field("writer", &self.get_ref())
            .field(
                "buffer",
                &format_args!("{}/{}", self.inner.buffer().len(), self.inner.capacity()),
            )
            .finish_non_exhaustive()
    }
}
