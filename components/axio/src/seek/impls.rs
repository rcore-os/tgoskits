#[cfg(feature = "alloc")]
use alloc::boxed::Box;

use crate::{Result, Seek, SeekFrom};

// =============================================================================
// Forwarding implementations

impl<S: Seek + ?Sized> Seek for &mut S {
    #[inline]
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        (**self).seek(pos)
    }

    #[inline]
    fn rewind(&mut self) -> Result<()> {
        (**self).rewind()
    }

    #[inline]
    fn stream_len(&mut self) -> Result<u64> {
        (**self).stream_len()
    }

    #[inline]
    fn stream_position(&mut self) -> Result<u64> {
        (**self).stream_position()
    }

    #[inline]
    fn seek_relative(&mut self, offset: i64) -> Result<()> {
        (**self).seek_relative(offset)
    }
}

#[cfg(feature = "alloc")]
impl<S: Seek + ?Sized> Seek for Box<S> {
    #[inline]
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        (**self).seek(pos)
    }

    #[inline]
    fn rewind(&mut self) -> Result<()> {
        (**self).rewind()
    }

    #[inline]
    fn stream_len(&mut self) -> Result<u64> {
        (**self).stream_len()
    }

    #[inline]
    fn stream_position(&mut self) -> Result<u64> {
        (**self).stream_position()
    }

    #[inline]
    fn seek_relative(&mut self, offset: i64) -> Result<()> {
        (**self).seek_relative(offset)
    }
}
