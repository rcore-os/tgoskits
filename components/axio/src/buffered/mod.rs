mod bufreader;
mod bufwriter;
mod linewriter;

use core::fmt;

pub use self::{
    bufreader::BufReader,
    bufwriter::{BufWriter, WriterPanicked},
    linewriter::LineWriter,
};
use crate::Error;

/// An error returned by [`BufWriter::into_inner`] which combines an error that
/// happened while writing out the buffer, and the buffered writer object
/// which may be used to recover from the condition.
#[derive(Debug)]
pub struct IntoInnerError<W>(W, Error);

impl<W> IntoInnerError<W> {
    /// Constructs a new IntoInnerError
    fn new(writer: W, error: Error) -> Self {
        Self(writer, error)
    }

    /// Helper to construct a new IntoInnerError; intended to help with
    /// adapters that wrap other adapters
    fn new_wrapped<W2>(self, f: impl FnOnce(W) -> W2) -> IntoInnerError<W2> {
        let Self(writer, error) = self;
        IntoInnerError::new(f(writer), error)
    }

    /// Returns the error which caused the call to [`BufWriter::into_inner()`]
    /// to fail.
    ///
    /// This error was returned when attempting to write the internal buffer.
    pub fn error(&self) -> &Error {
        &self.1
    }

    /// Returns the buffered writer instance which generated the error.
    ///
    /// The returned object can be used for error recovery, such as
    /// re-inspecting the buffer.
    pub fn into_inner(self) -> W {
        self.0
    }

    /// Consumes the [`IntoInnerError`] and returns the error which caused the call to
    /// [`BufWriter::into_inner()`] to fail.  Unlike `error`, this can be used to
    /// obtain ownership of the underlying error.
    pub fn into_error(self) -> Error {
        self.1
    }

    /// Consumes the [`IntoInnerError`] and returns the error which caused the call to
    /// [`BufWriter::into_inner()`] to fail, and the underlying writer.
    ///
    /// This can be used to simply obtain ownership of the underlying error; it can also be used for
    /// advanced error recovery.
    pub fn into_parts(self) -> (Error, W) {
        (self.1, self.0)
    }
}

impl<W> From<IntoInnerError<W>> for Error {
    fn from(iie: IntoInnerError<W>) -> Error {
        iie.1
    }
}

impl<W> fmt::Display for IntoInnerError<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error().fmt(f)
    }
}
