use crate::{Read, Result, Write};

/// Reader created by [`read_fn`].
pub struct ReadFn<R> {
    r: R,
}

/// Creates a reader that wraps a function.
pub fn read_fn<R>(r: R) -> ReadFn<R>
where
    R: FnMut(&mut [u8]) -> Result<usize>,
{
    ReadFn { r }
}

impl<R> Read for ReadFn<R>
where
    R: FnMut(&mut [u8]) -> Result<usize>,
{
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        (self.r)(buf)
    }
}

/// Writer created by [`write_fn`].
pub struct WriteFn<W> {
    w: W,
}

/// Creates a writer that wraps a function.
pub fn write_fn<W>(w: W) -> WriteFn<W>
where
    W: FnMut(&[u8]) -> Result<usize>,
{
    WriteFn { w }
}

impl<W> Write for WriteFn<W>
where
    W: FnMut(&[u8]) -> Result<usize>,
{
    #[inline]
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        (self.w)(buf)
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
