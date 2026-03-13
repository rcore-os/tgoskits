#[cfg(feature = "alloc")]
use alloc::{string::String, vec::Vec};
use core::io::BorrowedCursor;

use crate::{Error, Result};

mod impls;

/// Default [`Read::read_exact`] implementation.
pub fn default_read_exact<R: Read + ?Sized>(this: &mut R, mut buf: &mut [u8]) -> Result<()> {
    while !buf.is_empty() {
        match this.read(buf) {
            Ok(0) => break,
            Ok(n) => {
                buf = &mut buf[n..];
            }
            #[cfg(feature = "continue-on-interrupt")]
            Err(e) if e.canonicalize() == Error::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    if !buf.is_empty() {
        Err(Error::UnexpectedEof)
    } else {
        Ok(())
    }
}

/// Default [`Read::read_buf`] implementation.
pub fn default_read_buf<F>(read: F, mut cursor: BorrowedCursor<'_>) -> Result<()>
where
    F: FnOnce(&mut [u8]) -> Result<usize>,
{
    #[cfg(borrowedbuf_init)]
    {
        let n = read(cursor.ensure_init().init_mut())?;
        cursor.advance(n);
    }
    #[cfg(not(borrowedbuf_init))]
    {
        // SAFETY: We do not uninitialize any part of the buffer.
        let n = read(unsafe { cursor.as_mut().write_filled(0) })?;
        assert!(n <= cursor.capacity());
        // SAFETY: We've initialized the entire buffer, and `read` can't make it uninitialized.
        unsafe {
            cursor.advance(n);
        }
    }
    Ok(())
}

/// Default [`Read::read_buf_exact`] implementation.
pub fn default_read_buf_exact<R: Read + ?Sized>(
    this: &mut R,
    mut cursor: BorrowedCursor<'_>,
) -> Result<()> {
    while cursor.capacity() > 0 {
        let prev_written = cursor.written();
        match this.read_buf(cursor.reborrow()) {
            Ok(()) => {}
            #[cfg(feature = "continue-on-interrupt")]
            Err(e) if e.canonicalize() == Error::Interrupted => continue,
            Err(e) => return Err(e),
        }

        if cursor.written() == prev_written {
            return Err(Error::UnexpectedEof);
        }
    }

    Ok(())
}

/// Default [`Read::read_to_end`] implementation with optional size hint.
#[cfg(feature = "alloc")]
pub fn default_read_to_end<R: Read + ?Sized>(
    r: &mut R,
    buf: &mut Vec<u8>,
    size_hint: Option<usize>,
) -> Result<usize> {
    use core::io::BorrowedBuf;

    use crate::DEFAULT_BUF_SIZE;

    let start_len = buf.len();
    let start_cap = buf.capacity();
    // Optionally limit the maximum bytes read on each iteration.
    // This adds an arbitrary fiddle factor to allow for more data than we expect.
    let mut max_read_size = size_hint
        .and_then(|s| {
            s.checked_add(1024)?
                .checked_next_multiple_of(DEFAULT_BUF_SIZE)
        })
        .unwrap_or(DEFAULT_BUF_SIZE);

    const PROBE_SIZE: usize = 32;

    fn small_probe_read<R: Read + ?Sized>(r: &mut R, buf: &mut Vec<u8>) -> Result<usize> {
        let mut probe = [0u8; PROBE_SIZE];

        #[allow(clippy::never_loop)]
        loop {
            match r.read(&mut probe) {
                Ok(n) => {
                    // there is no way to recover from allocation failure here
                    // because the data has already been read.
                    buf.extend_from_slice(&probe[..n]);
                    return Ok(n);
                }
                #[cfg(feature = "continue-on-interrupt")]
                Err(e) if e.canonicalize() == Error::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    if (size_hint.is_none() || size_hint == Some(0)) && buf.capacity() - buf.len() < PROBE_SIZE {
        let read = small_probe_read(r, buf)?;

        if read == 0 {
            return Ok(0);
        }
    }

    #[cfg(borrowedbuf_init)]
    let mut initialized = 0; // Extra initialized bytes from previous loop iteration
    #[cfg(borrowedbuf_init)]
    let mut consecutive_short_reads = 0;

    loop {
        if buf.len() == buf.capacity() && buf.capacity() == start_cap {
            // The buffer might be an exact fit. Let's read into a probe buffer
            // and see if it returns `Ok(0)`. If so, we've avoided an
            // unnecessary doubling of the capacity. But if not, append the
            // probe buffer to the primary buffer and let its capacity grow.
            let read = small_probe_read(r, buf)?;

            if read == 0 {
                return Ok(buf.len() - start_len);
            }
        }

        if buf.len() == buf.capacity() {
            // buf is full, need more space
            buf.try_reserve(PROBE_SIZE).map_err(|_| Error::NoMemory)?;
        }

        let mut spare = buf.spare_capacity_mut();
        let buf_len = spare.len().min(max_read_size);
        spare = &mut spare[..buf_len];
        let mut read_buf: BorrowedBuf<'_> = spare.into();

        #[cfg(borrowedbuf_init)]
        // SAFETY: These bytes were initialized but not filled in the previous loop
        unsafe {
            read_buf.set_init(initialized);
        }

        let mut cursor = read_buf.unfilled();
        #[allow(clippy::never_loop)]
        let result = loop {
            match r.read_buf(cursor.reborrow()) {
                #[cfg(feature = "continue-on-interrupt")]
                Err(e) if e.canonicalize() == Error::Interrupted => continue,
                // Do not stop now in case of error: we might have received both data
                // and an error
                res => break res,
            }
        };

        #[cfg(borrowedbuf_init)]
        let unfilled_but_initialized = cursor.init_mut().len();
        let bytes_read = cursor.written();
        #[cfg(borrowedbuf_init)]
        let was_fully_initialized = read_buf.init_len() == buf_len;

        // SAFETY: BorrowedBuf's invariants mean this much memory is initialized.
        unsafe {
            let new_len = bytes_read + buf.len();
            buf.set_len(new_len);
        }

        // Now that all data is pushed to the vector, we can fail without data loss
        result?;

        if bytes_read == 0 {
            return Ok(buf.len() - start_len);
        }

        #[cfg(borrowedbuf_init)]
        if bytes_read < buf_len {
            consecutive_short_reads += 1;
        } else {
            consecutive_short_reads = 0;
        }

        #[cfg(borrowedbuf_init)]
        {
            // store how much was initialized but not filled
            initialized = unfilled_but_initialized;
        }

        // Use heuristics to determine the max read size if no initial size hint was provided
        if size_hint.is_none() {
            #[cfg(borrowedbuf_init)]
            // The reader is returning short reads but it doesn't call ensure_init().
            // In that case we no longer need to restrict read sizes to avoid
            // initialization costs.
            // When reading from disk we usually don't get any short reads except at EOF.
            // So we wait for at least 2 short reads before uncapping the read buffer;
            // this helps with the Windows issue.
            if !was_fully_initialized && consecutive_short_reads > 1 {
                max_read_size = usize::MAX;
            }

            // we have passed a larger buffer than previously and the
            // reader still hasn't returned a short read
            if buf_len >= max_read_size && bytes_read == buf_len {
                max_read_size = max_read_size.saturating_mul(2);
            }
        }
    }
}

#[cfg(feature = "alloc")]
pub(crate) unsafe fn append_to_string<F>(buf: &mut String, f: F) -> Result<usize>
where
    F: FnOnce(&mut Vec<u8>) -> Result<usize>,
{
    struct Guard<'a> {
        buf: &'a mut Vec<u8>,
        len: usize,
    }

    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            unsafe {
                self.buf.set_len(self.len);
            }
        }
    }

    let mut g = Guard {
        len: buf.len(),
        buf: unsafe { buf.as_mut_vec() },
    };
    let ret = f(g.buf);

    // SAFETY: the caller promises to only append data to `buf`
    let appended = unsafe { g.buf.get_unchecked(g.len..) };
    if str::from_utf8(appended).is_err() {
        ret.and(Err(Error::IllegalBytes))
    } else {
        g.len = g.buf.len();
        ret
    }
}

/// Default [`Read::read_to_string`] implementation with optional size hint.
#[cfg(feature = "alloc")]
pub fn default_read_to_string<R: Read + ?Sized>(
    r: &mut R,
    buf: &mut String,
    size_hint: Option<usize>,
) -> Result<usize> {
    // Note that we do *not* call `r.read_to_end()` here. We are passing
    // `&mut Vec<u8>` (the raw contents of `buf`) into the `read_to_end`
    // method to fill it up. An arbitrary implementation could overwrite the
    // entire contents of the vector, not just append to it (which is what
    // we are expecting).
    //
    // To prevent extraneously checking the UTF-8-ness of the entire buffer
    // we pass it to our hardcoded `default_read_to_end` implementation which
    // we know is guaranteed to only read data into the end of the buffer.
    unsafe { append_to_string(buf, |b| default_read_to_end(r, b, size_hint)) }
}

/// The `Read` trait allows for reading bytes from a source.
///
/// See [`std::io::Read`] for more details.
pub trait Read {
    /// Pull some bytes from this source into the specified buffer, returning
    /// how many bytes were read.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Read the exact number of bytes required to fill `buf`.
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        default_read_exact(self, buf)
    }

    /// Pull some bytes from this source into the specified buffer.
    ///
    /// This method makes it possible to return both data and an error but it is advised against.
    fn read_buf(&mut self, buf: BorrowedCursor<'_>) -> Result<()> {
        default_read_buf(|b| self.read(b), buf)
    }

    /// Reads the exact number of bytes required to fill `cursor`.
    ///
    /// If this function returns an error, all bytes read will be appended to `cursor`.
    fn read_buf_exact(&mut self, cursor: BorrowedCursor<'_>) -> Result<()> {
        default_read_buf_exact(self, cursor)
    }

    /// Read all bytes until EOF in this source, placing them into `buf`.
    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        default_read_to_end(self, buf, None)
    }

    /// Read all bytes until EOF in this source, appending them to `buf`.
    #[cfg(feature = "alloc")]
    fn read_to_string(&mut self, buf: &mut String) -> Result<usize> {
        default_read_to_string(self, buf, None)
    }

    /// Creates a "by reference" adapter for this instance of `Read`.
    ///
    /// The returned `adapter` also implements Read and will simply borrow this
    /// current reader.
    fn by_ref(&mut self) -> &mut Self
    where
        Self: Sized,
    {
        self
    }
}

/// Reads all bytes from a [reader][Read] into a new [`String`].
///
/// This is a convenience function for [`Read::read_to_string`].
///
/// See [`std::io::read_to_string`] for more details.
#[cfg(feature = "alloc")]
pub fn read_to_string<R: Read>(mut reader: R) -> Result<String> {
    let mut buf = String::new();
    reader.read_to_string(&mut buf)?;
    Ok(buf)
}

/// A `BufRead` is a type of `Read`er which has an internal buffer, allowing it
/// to perform extra ways of reading.
///
/// See [`std::io::BufRead`] for more details.
pub trait BufRead: Read {
    /// Returns the contents of the internal buffer, filling it with more data, via `Read` methods,
    /// if empty.
    fn fill_buf(&mut self) -> Result<&[u8]>;

    /// Marks the given `amount` of additional bytes from the internal buffer as having been read.
    /// Subsequent calls to `read` only return bytes that have not been marked as read.
    fn consume(&mut self, amount: usize);

    /// Checks if there is any data left to be `read`.
    fn has_data_left(&mut self) -> Result<bool> {
        self.fill_buf().map(|b| !b.is_empty())
    }

    /// Skips all bytes until the delimiter `byte` or EOF is reached.
    fn skip_until(&mut self, byte: u8) -> Result<usize> {
        let mut read = 0;
        loop {
            let (done, used) = {
                let available = self.fill_buf()?;
                match memchr::memchr(byte, available) {
                    Some(i) => (true, i + 1),
                    None => (false, available.len()),
                }
            };
            self.consume(used);
            read += used;
            if done || used == 0 {
                return Ok(read);
            }
        }
    }

    /// Read all bytes into `buf` until the delimiter `byte` or EOF is reached.
    #[cfg(feature = "alloc")]
    fn read_until(&mut self, byte: u8, buf: &mut Vec<u8>) -> Result<usize> {
        let mut read = 0;
        loop {
            let (done, used) = {
                let available = self.fill_buf()?;
                match memchr::memchr(byte, available) {
                    Some(i) => {
                        buf.extend_from_slice(&available[..=i]);
                        (true, i + 1)
                    }
                    None => {
                        buf.extend_from_slice(available);
                        (false, available.len())
                    }
                }
            };
            self.consume(used);
            read += used;
            if done || used == 0 {
                return Ok(read);
            }
        }
    }

    /// Read all bytes until a newline (the `0xA` byte) is reached, and append
    /// them to the provided `String` buffer.
    #[cfg(feature = "alloc")]
    fn read_line(&mut self, buf: &mut String) -> Result<usize> {
        unsafe { super::append_to_string(buf, |b| self.read_until(b'\n', b)) }
    }

    /// Returns an iterator over the contents of this reader split on the byte
    /// `byte`.
    #[cfg(feature = "alloc")]
    fn split(self, byte: u8) -> Split<Self>
    where
        Self: Sized,
    {
        Split {
            buf: self,
            delim: byte,
        }
    }

    /// Returns an iterator over the lines of this reader.
    #[cfg(feature = "alloc")]
    fn lines(self) -> Lines<Self>
    where
        Self: Sized,
    {
        Lines { buf: self }
    }
}

/// An iterator over the contents of an instance of `BufRead` split on a
/// particular byte.
///
/// This struct is generally created by calling [`split`] on a `BufRead`.
/// Please see the documentation of [`split`] for more details.
///
/// [`split`]: BufRead::split
#[cfg(feature = "alloc")]
#[derive(Debug)]
pub struct Split<B> {
    buf: B,
    delim: u8,
}

#[cfg(feature = "alloc")]
impl<B: BufRead> Iterator for Split<B> {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Result<Vec<u8>>> {
        let mut buf = Vec::new();
        match self.buf.read_until(self.delim, &mut buf) {
            Ok(0) => None,
            Ok(_n) => {
                if buf[buf.len() - 1] == self.delim {
                    buf.pop();
                }
                Some(Ok(buf))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

/// An iterator over the lines of an instance of `BufRead`.
///
/// This struct is generally created by calling [`lines`] on a `BufRead`.
/// Please see the documentation of [`lines`] for more details.
///
/// [`lines`]: BufRead::lines
#[cfg(feature = "alloc")]
#[derive(Debug)]
pub struct Lines<B> {
    buf: B,
}

#[cfg(feature = "alloc")]
impl<B: BufRead> Iterator for Lines<B> {
    type Item = Result<String>;

    fn next(&mut self) -> Option<Result<String>> {
        let mut buf = String::new();
        match self.buf.read_line(&mut buf) {
            Ok(0) => None,
            Ok(_n) => {
                if buf.ends_with('\n') {
                    buf.pop();
                    if buf.ends_with('\r') {
                        buf.pop();
                    }
                }
                Some(Ok(buf))
            }
            Err(e) => Some(Err(e)),
        }
    }
}
