#[cfg(feature = "alloc")]
use alloc::{boxed::Box, string::String, vec::Vec};
use core::{cmp, io::BorrowedCursor};

use crate::{BufRead, Error, IoBuf, IoBufMut, Read, Result, Seek, SeekFrom, Write};

/// A `Cursor` wraps an in-memory buffer and provides it with a
/// [`Seek`] implementation.
///
/// `Cursor`s are used with in-memory buffers, anything implementing
/// <code>[AsRef]<\[u8]></code>, to allow them to implement [`Read`] and/or [`Write`],
/// allowing these buffers to be used anywhere you might use a reader or writer
/// that does actual I/O.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Cursor<T> {
    inner: T,
    pos: u64,
}

impl<T> Cursor<T> {
    /// Creates a new cursor wrapping the provided underlying in-memory buffer.
    ///
    /// Cursor initial position is `0` even if underlying buffer (e.g., [`Vec`])
    /// is not empty. So writing to cursor starts with overwriting [`Vec`]
    /// content, not with appending to it.
    pub const fn new(inner: T) -> Cursor<T> {
        Cursor { pos: 0, inner }
    }

    /// Consumes this cursor, returning the underlying value.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Gets a reference to the underlying value in this cursor.
    pub const fn get_ref(&self) -> &T {
        &self.inner
    }

    /// Gets a mutable reference to the underlying value in this cursor.
    ///
    /// Care should be taken to avoid modifying the internal I/O state of the
    /// underlying value as it may corrupt this cursor's position.
    pub const fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Returns the current position of this cursor.
    pub const fn position(&self) -> u64 {
        self.pos
    }

    /// Sets the position of this cursor.
    pub const fn set_position(&mut self, pos: u64) {
        self.pos = pos;
    }
}

impl<T> Cursor<T>
where
    T: AsRef<[u8]>,
{
    /// Splits the underlying slice at the cursor position and returns them.
    pub fn split(&self) -> (&[u8], &[u8]) {
        let slice = self.inner.as_ref();
        let pos = self.pos.min(slice.len() as u64);
        slice.split_at(pos as usize)
    }
}

impl<T> Cursor<T>
where
    T: AsMut<[u8]>,
{
    /// Splits the underlying slice at the cursor position and returns them
    /// mutably.
    pub fn split_mut(&mut self) -> (&mut [u8], &mut [u8]) {
        let slice = self.inner.as_mut();
        let pos = self.pos.min(slice.len() as u64);
        slice.split_at_mut(pos as usize)
    }
}

impl<T> Clone for Cursor<T>
where
    T: Clone,
{
    #[inline]
    fn clone(&self) -> Self {
        Cursor {
            inner: self.inner.clone(),
            pos: self.pos,
        }
    }

    #[inline]
    fn clone_from(&mut self, other: &Self) {
        self.inner.clone_from(&other.inner);
        self.pos = other.pos;
    }
}

impl<T> Seek for Cursor<T>
where
    T: AsRef<[u8]>,
{
    fn seek(&mut self, style: SeekFrom) -> Result<u64> {
        let (base_pos, offset) = match style {
            SeekFrom::Start(n) => {
                self.pos = n;
                return Ok(n);
            }
            SeekFrom::End(n) => (self.inner.as_ref().len() as u64, n),
            SeekFrom::Current(n) => (self.pos, n),
        };
        match base_pos.checked_add_signed(offset) {
            Some(n) => {
                self.pos = n;
                Ok(self.pos)
            }
            None => Err(Error::InvalidInput),
        }
    }

    fn stream_len(&mut self) -> Result<u64> {
        Ok(self.inner.as_ref().len() as u64)
    }

    fn stream_position(&mut self) -> Result<u64> {
        Ok(self.pos)
    }
}

impl<T> Read for Cursor<T>
where
    T: AsRef<[u8]>,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = Read::read(&mut Cursor::split(self).1, buf)?;
        self.pos += n as u64;
        Ok(n)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        let result = Read::read_exact(&mut Cursor::split(self).1, buf);

        match result {
            Ok(_) => self.pos += buf.len() as u64,
            // The only possible error condition is EOF, so place the cursor at "EOF"
            Err(_) => self.pos = self.inner.as_ref().len() as u64,
        }

        result
    }

    fn read_buf(&mut self, mut cursor: BorrowedCursor<'_, u8>) -> Result<()> {
        let prev_written = cursor.written();

        Read::read_buf(&mut Cursor::split(self).1, cursor.reborrow())?;

        self.pos += (cursor.written() - prev_written) as u64;

        Ok(())
    }

    fn read_buf_exact(&mut self, mut cursor: BorrowedCursor<'_, u8>) -> Result<()> {
        let prev_written = cursor.written();

        let result = Read::read_buf_exact(&mut Cursor::split(self).1, cursor.reborrow());
        self.pos += (cursor.written() - prev_written) as u64;

        result
    }

    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        let content = Cursor::split(self).1;
        let len = content.len();
        buf.try_reserve(len).map_err(|_| Error::NoMemory)?;
        buf.extend_from_slice(content);
        self.pos += len as u64;

        Ok(len)
    }

    #[cfg(feature = "alloc")]
    fn read_to_string(&mut self, buf: &mut String) -> Result<usize> {
        let content = str::from_utf8(Cursor::split(self).1).map_err(|_| Error::IllegalBytes)?;
        let len = content.len();
        buf.try_reserve(len).map_err(|_| Error::NoMemory)?;
        buf.push_str(content);
        self.pos += len as u64;

        Ok(len)
    }
}

impl<T> BufRead for Cursor<T>
where
    T: AsRef<[u8]>,
{
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(Cursor::split(self).1)
    }

    fn consume(&mut self, amt: usize) {
        self.pos += amt as u64;
    }
}

fn slice_write(pos_mut: &mut u64, slice: &mut [u8], buf: &[u8]) -> Result<usize> {
    let pos = cmp::min(*pos_mut, slice.len() as u64);
    let amt = (&mut slice[(pos as usize)..]).write(buf)?;
    *pos_mut += amt as u64;
    Ok(amt)
}

#[inline]
fn slice_write_all(pos_mut: &mut u64, slice: &mut [u8], buf: &[u8]) -> Result<()> {
    let n = slice_write(pos_mut, slice, buf)?;
    if n < buf.len() {
        Err(Error::WriteZero)
    } else {
        Ok(())
    }
}

/// Reserves the required space, and pads the vec with 0s if necessary.
#[cfg(feature = "alloc")]
fn reserve_and_pad(pos_mut: &mut u64, vec: &mut Vec<u8>, buf_len: usize) -> Result<usize> {
    let pos: usize = (*pos_mut).try_into().map_err(|_| Error::InvalidInput)?;

    // For safety reasons, we don't want these numbers to overflow
    // otherwise our allocation won't be enough
    let desired_cap = pos.saturating_add(buf_len);
    if desired_cap > vec.capacity() {
        // We want our vec's total capacity
        // to have room for (pos+buf_len) bytes. Reserve allocates
        // based on additional elements from the length, so we need to
        // reserve the difference
        vec.reserve(desired_cap - vec.len());
    }
    // Pad if pos is above the current len.
    if pos > vec.len() {
        let diff = pos - vec.len();
        // Unfortunately, `resize()` would suffice but the optimiser does not
        // realise the `reserve` it does can be eliminated. So we do it manually
        // to eliminate that extra branch
        let spare = vec.spare_capacity_mut();
        debug_assert!(spare.len() >= diff);
        // Safety: we have allocated enough capacity for this.
        // And we are only writing, not reading
        unsafe {
            spare
                .get_unchecked_mut(..diff)
                .fill(core::mem::MaybeUninit::new(0));
            vec.set_len(pos);
        }
    }

    Ok(pos)
}

/// Writes the slice to the vec without allocating.
///
/// # Safety
///
/// `vec` must have `buf.len()` spare capacity.
#[cfg(feature = "alloc")]
unsafe fn vec_write_all_unchecked(pos: usize, vec: &mut Vec<u8>, buf: &[u8]) -> usize {
    debug_assert!(vec.capacity() >= pos + buf.len());
    unsafe { vec.as_mut_ptr().add(pos).copy_from(buf.as_ptr(), buf.len()) };
    pos + buf.len()
}

/// Resizing `write_all` implementation for [`Cursor`].
///
/// Cursor is allowed to have a pre-allocated and initialised
/// vector body, but with a position of 0. This means the [`Write`]
/// will overwrite the contents of the vec.
///
/// This also allows for the vec body to be empty, but with a position of N.
/// This means that [`Write`] will pad the vec with 0 initially,
/// before writing anything from that point
#[cfg(feature = "alloc")]
fn vec_write_all(pos_mut: &mut u64, vec: &mut Vec<u8>, buf: &[u8]) -> Result<usize> {
    let buf_len = buf.len();
    let mut pos = reserve_and_pad(pos_mut, vec, buf_len)?;

    // Write the buf then progress the vec forward if necessary
    // Safety: we have ensured that the capacity is available
    // and that all bytes get written up to pos
    unsafe {
        pos = vec_write_all_unchecked(pos, vec, buf);
        if pos > vec.len() {
            vec.set_len(pos);
        }
    };

    // Bump us forward
    *pos_mut += buf_len as u64;
    Ok(buf_len)
}

impl Write for Cursor<&mut [u8]> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        slice_write(&mut self.pos, self.inner, buf)
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        slice_write_all(&mut self.pos, self.inner, buf)
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "alloc")]
impl Write for Cursor<&mut Vec<u8>> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        vec_write_all(&mut self.pos, self.inner, buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        vec_write_all(&mut self.pos, self.inner, buf)?;
        Ok(())
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "alloc")]
impl Write for Cursor<Vec<u8>> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        vec_write_all(&mut self.pos, &mut self.inner, buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        vec_write_all(&mut self.pos, &mut self.inner, buf)?;
        Ok(())
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "alloc")]
impl Write for Cursor<Box<[u8]>> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        slice_write(&mut self.pos, &mut self.inner, buf)
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        slice_write_all(&mut self.pos, &mut self.inner, buf)
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl<const N: usize> Write for Cursor<[u8; N]> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        slice_write(&mut self.pos, &mut self.inner, buf)
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        slice_write_all(&mut self.pos, &mut self.inner, buf)
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl<T: IoBuf> IoBuf for Cursor<T> {
    #[inline]
    fn remaining(&self) -> usize {
        self.inner.remaining() - (self.pos as usize)
    }
}

impl<T: IoBufMut> IoBufMut for Cursor<T> {
    #[inline]
    fn remaining_mut(&self) -> usize {
        self.inner.remaining_mut() - (self.pos as usize)
    }
}

#[cfg(axtest)]
pub(crate) fn cursor_constructors_and_position_hold_for_test() -> bool {
    use alloc::vec;

    let cursor = crate::Cursor::new(vec![1, 2, 3, 4, 5]);
    assert_eq!(cursor.position(), 0);
    assert_eq!(*cursor.get_ref(), [1, 2, 3, 4, 5]);

    let mut cursor = crate::Cursor::new(vec![0u8; 0]);
    assert!(cursor.get_mut().is_empty());
    cursor.set_position(42);
    assert_eq!(cursor.position(), 42);

    // Test into_inner
    let cursor = crate::Cursor::new(vec![10, 20, 30]);
    let inner = cursor.into_inner();
    assert_eq!(inner, vec![10, 20, 30]);

    true
}

#[cfg(axtest)]
pub(crate) fn cursor_split_and_clone_hold_for_test() -> bool {
    use alloc::vec;

    // Test split method
    let cursor = crate::Cursor::new(vec![1, 2, 3, 4, 5]);
    let (before, after) = crate::Cursor::split(&cursor);
    assert!(before.is_empty()); // position is 0
    assert_eq!(after, [1, 2, 3, 4, 5]);

    // Test split at non-zero position
    let mut cursor = crate::Cursor::new(vec![1, 2, 3, 4, 5]);
    cursor.set_position(3);
    let (before, after) = crate::Cursor::split(&cursor);
    assert_eq!(before, [1, 2, 3]);
    assert_eq!(after, [4, 5]);

    // Test Clone implementation
    let cursor1 = crate::Cursor::new(vec![10, 20, 30]);
    let cursor2 = cursor1.clone();
    assert_eq!(cursor1.position(), cursor2.position());
    assert_eq!(*cursor1.get_ref(), *cursor2.get_ref());

    // Test Default implementation
    let cursor: crate::Cursor<alloc::vec::Vec<u8>> = Default::default();
    assert_eq!(cursor.position(), 0);
    assert!(cursor.get_ref().is_empty());

    true
}

#[cfg(axtest)]
pub(crate) fn cursor_seek_from_variants_hold_for_test() -> bool {
    use alloc::vec;
    use crate::{Cursor, Seek, SeekFrom};

    let mut cursor = Cursor::new(vec![1, 2, 3, 4, 5]);

    // Test SeekFrom::Start
    let pos = cursor.seek(SeekFrom::Start(2)).unwrap();
    assert_eq!(pos, 2);
    assert_eq!(cursor.position(), 2);

    // Test SeekFrom::Current
    let pos = cursor.seek(SeekFrom::Current(1)).unwrap();
    assert_eq!(pos, 3);
    assert_eq!(cursor.position(), 3);

    // Test SeekFrom::End
    let pos = cursor.seek(SeekFrom::End(-2)).unwrap();
    assert_eq!(pos, 3); // 5 - 2 = 3
    assert_eq!(cursor.position(), 3);

    // Test stream_len
    let len = cursor.stream_len().unwrap();
    assert_eq!(len, 5);

    // Test stream_position
    let pos = cursor.stream_position().unwrap();
    assert_eq!(pos, 3);

    true
}

#[cfg(axtest)]
pub(crate) fn cursor_split_mut_and_write_hold_for_test() -> bool {
    use alloc::vec;

    // Test split_mut method
    let mut cursor = crate::Cursor::new(vec![1, 2, 3, 4, 5]);
    cursor.set_position(2);
    let (before, after) = crate::Cursor::split_mut(&mut cursor);
    assert_eq!(before, [1, 2]);
    assert_eq!(after, [3, 4, 5]);

    // Test Write impl for Cursor<&mut [u8]>
    let mut buf = [0u8; 10];
    {
        let mut cursor = crate::Cursor::new(&mut buf[..]);
        cursor.write_all(&[1, 2, 3]).unwrap();
        assert_eq!(cursor.position(), 3);
    }
    assert_eq!(&buf[..3], [1, 2, 3]);

    // Test Write impl for Cursor<Vec<u8>>
    let mut vec = alloc::vec::Vec::new();
    {
        let mut cursor = crate::Cursor::new(&mut vec);
        cursor.write_all(&[4, 5, 6]).unwrap();
        assert_eq!(cursor.position(), 3);
    }
    assert_eq!(vec, [4, 5, 6]);

    true
}

#[cfg(axtest)]
pub(crate) fn cursor_buf_read_impl_hold_for_test() -> bool {
    use alloc::vec;
    use crate::{BufRead, Cursor};

    // Test BufRead impl for Cursor
    let mut cursor = Cursor::new(vec![1, 2, 3, 4, 5]);
    
    // Test fill_buf
    let buf = cursor.fill_buf().unwrap();
    assert_eq!(buf, &[1, 2, 3, 4, 5]);

    // Test Read impl
    let mut cursor = Cursor::new(vec![10, 20, 30, 40, 50]);
    let mut read_buf = [0u8; 3];
    let n = cursor.read(&mut read_buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(&read_buf, &[10, 20, 30]);
    assert_eq!(cursor.position(), 3);

    true
}

#[cfg(axtest)]
pub(crate) fn cursor_write_box_array_and_fixed_size_hold_for_test() -> bool {
    use alloc::boxed::Box;
    use crate::{Cursor, Write};

    // Test Write impl for Cursor<Box<[u8]>>
    let boxed: Box<[u8]> = Box::new([0u8; 16]);
    let mut cursor = Cursor::new(boxed);
    cursor.write_all(&[1, 2, 3]).unwrap();
    assert_eq!(cursor.position(), 3);

    // Test Write impl for Cursor<[u8; N]>
    let mut cursor = Cursor::new([0u8; 8]);
    cursor.write_all(&[4, 5, 6, 7]).unwrap();
    assert_eq!(cursor.position(), 4);
    
    // Test flush (should always succeed)
    let mut cursor = Cursor::new([0u8; 4]);
    cursor.flush().unwrap();

    true
}
