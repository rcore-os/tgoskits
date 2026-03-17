#[cfg(feature = "alloc")]
use alloc::{collections::vec_deque::VecDeque, vec::Vec};
use core::{
    io::{BorrowedBuf, BorrowedCursor},
    mem::MaybeUninit,
};

use crate::{BufReader, BufWriter, DEFAULT_BUF_SIZE, IoBuf, IoBufMut, Read, Result, Write};

/// Extension methods for [`IoBuf`].
pub trait IoBufExt: Read + IoBuf {
    /// Reads some bytes from this buffer and writes them into `writer`.
    #[inline]
    fn write_to<W: Write + ?Sized>(&mut self, writer: &mut W) -> Result<usize> {
        IoBufSpec::write_to(self, writer)
    }
}

impl<T: Read + IoBuf + ?Sized> IoBufExt for T {}

/// Extension methods for [`IoBufMut`].
pub trait IoBufMutExt: Write + IoBufMut {
    /// Reads some bytes from `reader` and writes them into this buffer.
    #[inline]
    fn read_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<usize> {
        IoBufMutSpec::read_from(self, reader)
    }
}

impl<T: Write + IoBufMut + ?Sized> IoBufMutExt for T {}

fn stack_buffer_transfer<R, W>(reader: &mut R, writer: &mut W, size_limit: usize) -> Result<usize>
where
    R: Read + ?Sized,
    W: Write + ?Sized,
{
    let mut read_buf = [MaybeUninit::uninit(); DEFAULT_BUF_SIZE];

    let limit = read_buf.len().min(size_limit);
    let mut buf: BorrowedBuf<'_> = (&mut read_buf[..limit]).into();

    reader.read_buf(buf.unfilled())?;

    if buf.len() == 0 {
        return Ok(0);
    }

    writer.write(buf.filled())
}

trait IoBufSpec {
    fn write_to<W: Write + ?Sized>(&mut self, writer: &mut W) -> Result<usize>;
}

impl<R: Read + IoBuf + ?Sized> IoBufSpec for R {
    default fn write_to<W: Write + ?Sized>(&mut self, writer: &mut W) -> Result<usize> {
        stack_buffer_transfer(self, writer, self.remaining())
    }
}

impl IoBufSpec for &[u8] {
    fn write_to<W: Write + ?Sized>(&mut self, writer: &mut W) -> Result<usize> {
        writer.write(self)
    }
}

#[cfg(feature = "alloc")]
impl IoBufSpec for VecDeque<u8> {
    fn write_to<W: Write + ?Sized>(&mut self, writer: &mut W) -> Result<usize> {
        let (front, _back) = self.as_slices();
        let written = writer.write(front)?;
        self.drain(..written);
        Ok(written)
    }
}

impl<I: ?Sized> IoBufSpec for BufReader<I>
where
    Self: Read + IoBuf,
{
    fn write_to<W: Write + ?Sized>(&mut self, writer: &mut W) -> Result<usize> {
        // Hack: this relies on `impl Read for BufReader` always calling fill_buf
        // if the buffer is empty, even for empty slices.
        // It can't be called directly here since specialization prevents us
        // from adding I: Read
        self.read(&mut [])?;

        let buf = self.buffer();
        if buf.is_empty() {
            return Ok(0);
        }

        let written = writer.write(buf)?;
        self.consume(written);
        Ok(written)
    }
}

trait IoBufMutSpec {
    fn read_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<usize>;
}

impl<W: Write + IoBufMut + ?Sized> IoBufMutSpec for W {
    default fn read_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<usize> {
        stack_buffer_transfer(reader, self, self.remaining_mut())
    }
}

impl IoBufMutSpec for &mut [u8] {
    fn read_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<usize> {
        reader.read(self)
    }
}

macro_rules! read_from_vec_impl {
    ($buf:ident, $reader:ident) => {{
        let mut read_buf: BorrowedBuf<'_> = $buf.spare_capacity_mut().into();
        let result = $reader.read_buf(read_buf.unfilled());
        let bytes_read = read_buf.len();
        unsafe {
            $buf.set_len($buf.len() + bytes_read);
        }
        result.map(|()| bytes_read)
    }};
}

#[cfg(feature = "alloc")]
impl IoBufMutSpec for Vec<u8> {
    #[inline]
    fn read_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<usize> {
        read_from_vec_impl!(self, reader)
    }
}

impl IoBufMutSpec for BorrowedCursor<'_> {
    fn read_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<usize> {
        reader.read_buf(self.reborrow())?;
        Ok(self.written())
    }
}

impl<I: Write + ?Sized> IoBufMutSpec for BufWriter<I>
where
    Self: IoBufMut,
{
    fn read_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<usize> {
        let buf = self.buffer_mut();
        read_from_vec_impl!(buf, reader)
    }
}
