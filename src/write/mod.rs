use core::fmt;

use crate::{Error, Result};

mod impls;

pub(crate) fn default_write_fmt<W: Write + ?Sized>(
    this: &mut W,
    args: fmt::Arguments<'_>,
) -> Result<()> {
    // Create a shim which translates a `Write` to a `fmt::Write` and saves off
    // I/O errors, instead of discarding them.
    struct Adapter<'a, T: ?Sized + 'a> {
        inner: &'a mut T,
        error: Result<()>,
    }

    impl<T: Write + ?Sized> fmt::Write for Adapter<'_, T> {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            match self.inner.write_all(s.as_bytes()) {
                Ok(()) => Ok(()),
                Err(e) => {
                    self.error = Err(e);
                    Err(fmt::Error)
                }
            }
        }
    }

    let mut output = Adapter {
        inner: this,
        error: Ok(()),
    };
    match fmt::write(&mut output, args) {
        Ok(()) => Ok(()),
        Err(..) => {
            // Check whether the error came from the underlying `Write`.
            if output.error.is_err() {
                output.error
            } else {
                // This shouldn't happen: the underlying stream did not error,
                // but somehow the formatter still errored?
                panic!(
                    "a formatting trait implementation returned an error when the underlying \
                     stream did not"
                );
            }
        }
    }
}

/// A trait for objects which are byte-oriented sinks.
///
/// See [`std::io::Write`] for more details.
pub trait Write {
    /// Write a buffer into this writer, returning how many bytes were written.
    fn write(&mut self, buf: &[u8]) -> Result<usize>;

    /// Flush this output stream, ensuring that all intermediately buffered
    /// contents reach their destination.
    fn flush(&mut self) -> Result<()>;

    /// Attempts to write an entire buffer into this writer.
    fn write_all(&mut self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.write(buf) {
                Ok(0) => return Err(Error::WriteZero),
                Ok(n) => buf = &buf[n..],
                #[cfg(feature = "continue-on-interrupt")]
                Err(e) if e.canonicalize() == Error::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Writes a formatted string into this writer, returning any error
    /// encountered.
    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> Result<()> {
        // NOTE: std's implementation uses an internal method
        // `as_statically_known_str` to optimize.
        default_write_fmt(self, args)
    }

    /// Creates a "by reference" adapter for this instance of `Write`.
    ///
    /// The returned adapter also implements `Write` and will simply borrow this
    /// current writer.
    fn by_ref(&mut self) -> &mut Self
    where
        Self: Sized,
    {
        self
    }
}
