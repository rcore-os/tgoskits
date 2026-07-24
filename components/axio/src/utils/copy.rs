#[cfg(feature = "alloc")]
use alloc::{collections::vec_deque::VecDeque, vec::Vec};
use core::{io::BorrowedBuf, mem::MaybeUninit};

use crate::{BufReader, BufWriter, DEFAULT_BUF_SIZE, Error, Read, Result, Write};

/// Copies the entire contents of a reader into a writer.
///
/// This function will continuously read data from `reader` and then
/// write it into `writer` in a streaming fashion until `reader`
/// returns EOF.
///
/// On success, the total number of bytes that were copied from
/// `reader` to `writer` is returned.
///
/// See [`std::io::copy`] for more details.
pub fn copy<R, W>(reader: &mut R, writer: &mut W) -> Result<u64>
where
    R: Read + ?Sized,
    W: Write + ?Sized,
{
    let read_buf = BufferedReaderSpec::buffer_size(reader);
    let write_buf = BufferedWriterSpec::buffer_size(writer);

    if read_buf >= DEFAULT_BUF_SIZE && read_buf >= write_buf {
        return BufferedReaderSpec::copy_to(reader, writer);
    }

    BufferedWriterSpec::copy_from(writer, reader)
}

/// Fallback [`copy`] implementation using a stack-allocated buffer.
pub fn stack_buffer_copy<R, W>(reader: &mut R, writer: &mut W) -> Result<u64>
where
    R: Read + ?Sized,
    W: Write + ?Sized,
{
    let buf: &mut [_] = &mut [MaybeUninit::uninit(); DEFAULT_BUF_SIZE];
    let mut buf: BorrowedBuf<'_, u8> = buf.into();

    let mut len = 0;

    loop {
        match reader.read_buf(buf.unfilled()) {
            Ok(()) => {}
            Err(e) if e.canonicalize() == Error::Interrupted => continue,
            Err(e) => return Err(e),
        };

        if buf.filled().is_empty() {
            break;
        }

        len += buf.filled().len() as u64;
        writer.write_all(buf.filled())?;
        buf.clear();
    }

    Ok(len)
}

/// Specialization of the read-write loop that reuses the internal
/// buffer of a BufReader. If there's no buffer then the writer side
/// should be used instead.
trait BufferedReaderSpec {
    fn buffer_size(&self) -> usize;

    fn copy_to(&mut self, to: &mut (impl Write + ?Sized)) -> Result<u64>;
}

impl<T> BufferedReaderSpec for T
where
    Self: Read,
    T: ?Sized,
{
    #[inline]
    default fn buffer_size(&self) -> usize {
        0
    }

    default fn copy_to(&mut self, _to: &mut (impl Write + ?Sized)) -> Result<u64> {
        unreachable!("only called from specializations")
    }
}

impl BufferedReaderSpec for &[u8] {
    fn buffer_size(&self) -> usize {
        // prefer this specialization since the source "buffer" is all we'll ever need,
        // even if it's small
        usize::MAX
    }

    fn copy_to(&mut self, to: &mut (impl Write + ?Sized)) -> Result<u64> {
        let len = self.len();
        to.write_all(self)?;
        *self = &self[len..];
        Ok(len as u64)
    }
}

#[cfg(feature = "alloc")]
impl BufferedReaderSpec for VecDeque<u8> {
    fn buffer_size(&self) -> usize {
        // prefer this specialization since the source "buffer" is all we'll ever need,
        // even if it's small
        usize::MAX
    }

    fn copy_to(&mut self, to: &mut (impl Write + ?Sized)) -> Result<u64> {
        let len = self.len();
        let (front, back) = self.as_slices();
        to.write_all(front)?;
        to.write_all(back)?;
        self.clear();
        Ok(len as u64)
    }
}

impl<I> BufferedReaderSpec for BufReader<I>
where
    Self: Read,
    I: ?Sized,
{
    fn buffer_size(&self) -> usize {
        self.capacity()
    }

    fn copy_to(&mut self, to: &mut (impl Write + ?Sized)) -> Result<u64> {
        let mut len = 0;

        loop {
            // Hack: this relies on `impl Read for BufReader` always calling fill_buf
            // if the buffer is empty, even for empty slices.
            // It can't be called directly here since specialization prevents us
            // from adding I: Read
            match self.read(&mut []) {
                Ok(_) => {}
                Err(e) if e.canonicalize() == Error::Interrupted => continue,
                Err(e) => return Err(e),
            }
            let buf = self.buffer();
            if self.buffer().is_empty() {
                return Ok(len);
            }

            // In case the writer side is a BufWriter then its write_all
            // implements an optimization that passes through large
            // buffers to the underlying writer. That code path is #[cold]
            // but we're still avoiding redundant memcopies when doing
            // a copy between buffered inputs and outputs.
            to.write_all(buf)?;
            len += buf.len() as u64;
            self.discard_buffer();
        }
    }
}

/// Specialization of the read-write loop that either uses a stack buffer
/// or reuses the internal buffer of a BufWriter
trait BufferedWriterSpec: Write {
    fn buffer_size(&self) -> usize;

    fn copy_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<u64>;
}

impl<W: Write + ?Sized> BufferedWriterSpec for W {
    #[inline]
    default fn buffer_size(&self) -> usize {
        0
    }

    default fn copy_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<u64> {
        stack_buffer_copy(reader, self)
    }
}

#[cfg(feature = "alloc")]
impl BufferedWriterSpec for Vec<u8> {
    fn buffer_size(&self) -> usize {
        core::cmp::max(DEFAULT_BUF_SIZE, self.capacity() - self.len())
    }

    fn copy_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<u64> {
        reader
            .read_to_end(self)
            .map(|bytes| u64::try_from(bytes).expect("usize overflowed u64"))
    }
}

impl<I: Write + ?Sized> BufferedWriterSpec for BufWriter<I> {
    fn buffer_size(&self) -> usize {
        self.capacity()
    }

    fn copy_from<R: Read + ?Sized>(&mut self, reader: &mut R) -> Result<u64> {
        if self.capacity() < DEFAULT_BUF_SIZE {
            return stack_buffer_copy(reader, self);
        }

        let mut len = 0;
        let mut init = false;

        loop {
            let buf = self.buffer_mut();
            let mut read_buf: BorrowedBuf<'_, u8> = buf.spare_capacity_mut().into();

            if init {
                // SAFETY: init is either 0 or the init_len from the previous iteration.
                unsafe { read_buf.set_init() };
            }

            if read_buf.capacity() >= DEFAULT_BUF_SIZE {
                let mut cursor = read_buf.unfilled();
                match reader.read_buf(cursor.reborrow()) {
                    Ok(()) => {
                        let bytes_read = cursor.written();

                        if bytes_read == 0 {
                            return Ok(len);
                        }

                        init = read_buf.is_init();
                        len += bytes_read as u64;

                        // SAFETY: BorrowedBuf guarantees all of its filled bytes are init
                        unsafe { buf.set_len(buf.len() + bytes_read) };

                        // Read again if the buffer still has enough capacity, as BufWriter itself
                        // would do This will occur if the reader returns
                        // short reads
                    }
                    Err(ref e) if e.canonicalize() == Error::Interrupted => {}
                    Err(e) => return Err(e),
                }
            } else {
                self.flush_buf()?;
            }
        }
    }
}

#[cfg(axtest)]
struct AxtestShortReader<'a> {
    remaining: &'a [u8],
    max_read: usize,
    largest_request: usize,
}

#[cfg(axtest)]
impl Read for AxtestShortReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.largest_request = self.largest_request.max(buf.len());

        let bytes = self.remaining.len().min(self.max_read).min(buf.len());
        let (copied, remaining) = self.remaining.split_at(bytes);
        buf[..bytes].copy_from_slice(copied);
        self.remaining = remaining;

        Ok(bytes)
    }
}

#[cfg(axtest)]
struct AxtestFixedWriter<'a> {
    output: &'a mut [u8],
    written: usize,
    largest_write: usize,
}

#[cfg(axtest)]
impl AxtestFixedWriter<'_> {
    fn filled(&self) -> &[u8] {
        &self.output[..self.written]
    }
}

#[cfg(axtest)]
impl Write for AxtestFixedWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.largest_write = self.largest_write.max(buf.len());

        let bytes = (self.output.len() - self.written).min(buf.len());
        let end = self.written + bytes;
        self.output[self.written..end].copy_from_slice(&buf[..bytes]);
        self.written = end;

        Ok(bytes)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(axtest)]
/// Verifies the stack-buffer copy path with short reads and writer failure.
pub fn copy_constants_hold_for_test() -> bool {
    let source = *b"stack-buffer-copy";
    let mut reader = AxtestShortReader {
        remaining: &source,
        max_read: 3,
        largest_request: 0,
    };
    let mut output = [0; 17];
    let mut writer = AxtestFixedWriter {
        output: &mut output,
        written: 0,
        largest_write: 0,
    };

    assert_eq!(
        stack_buffer_copy(&mut reader, &mut writer),
        Ok(source.len() as u64)
    );
    assert_eq!(writer.filled(), source);
    assert!(reader.remaining.is_empty());
    assert_eq!(reader.largest_request, DEFAULT_BUF_SIZE);

    let mut reader = AxtestShortReader {
        remaining: &source,
        max_read: source.len(),
        largest_request: 0,
    };
    let mut short_output = [0; 4];
    let mut short_writer = AxtestFixedWriter {
        output: &mut short_output,
        written: 0,
        largest_write: 0,
    };

    assert_eq!(
        stack_buffer_copy(&mut reader, &mut short_writer),
        Err(Error::WriteZero)
    );

    true
}

#[cfg(axtest)]
/// Verifies copy dispatches through the buffered-reader specialization.
pub fn copy_buffered_reader_spec_hold_for_test() -> bool {
    let source = [0x5a; DEFAULT_BUF_SIZE * 2 + 11];
    let mut reader = BufReader::with_capacity(
        DEFAULT_BUF_SIZE * 2,
        AxtestShortReader {
            remaining: &source,
            max_read: source.len(),
            largest_request: 0,
        },
    );
    let mut output = [0; DEFAULT_BUF_SIZE * 2 + 11];
    let mut writer = AxtestFixedWriter {
        output: &mut output,
        written: 0,
        largest_write: 0,
    };

    assert_eq!(copy(&mut reader, &mut writer), Ok(source.len() as u64));
    assert_eq!(writer.filled(), source);
    assert_eq!(writer.largest_write, DEFAULT_BUF_SIZE * 2);
    assert!(reader.into_inner().remaining.is_empty());

    true
}

#[cfg(axtest)]
/// Verifies copy dispatches through the byte-slice specialization.
pub fn copy_slice_specialization_hold_for_test() -> bool {
    let source = [0x7b; DEFAULT_BUF_SIZE + 17];
    let mut reader = source.as_slice();
    let mut output = [0; DEFAULT_BUF_SIZE + 17];
    let mut writer = AxtestFixedWriter {
        output: &mut output,
        written: 0,
        largest_write: 0,
    };

    assert_eq!(copy(&mut reader, &mut writer), Ok(source.len() as u64));
    assert_eq!(writer.filled(), source);
    assert_eq!(writer.largest_write, source.len());
    assert!(reader.is_empty());

    true
}
