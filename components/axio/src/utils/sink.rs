use core::fmt;

use crate::{IoBufMut, Result, Write};

/// A writer which will move data into the void.
///
/// This struct is generally created by calling [`sink()`]. Please
/// see the documentation of [`sink()`] for more details.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Default)]
pub struct Sink;

/// Creates an instance of a writer which will successfully consume all data.
#[must_use]
pub const fn sink() -> Sink {
    Sink
}

impl Write for Sink {
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

impl Write for &Sink {
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

impl IoBufMut for Sink {
    fn remaining_mut(&self) -> usize {
        usize::MAX
    }
}
