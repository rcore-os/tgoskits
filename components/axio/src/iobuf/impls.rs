#[cfg(feature = "alloc")]
use alloc::{boxed::Box, collections::VecDeque, vec::Vec};
use core::io::BorrowedCursor;

use crate::{IoBuf, IoBufMut};

// =============================================================================
// Forwarding implementations

impl<R: IoBuf + ?Sized> IoBuf for &R {
    #[inline]
    fn remaining(&self) -> usize {
        (**self).remaining()
    }
}

impl<W: IoBufMut + ?Sized> IoBufMut for &W {
    #[inline]
    fn remaining_mut(&self) -> usize {
        (**self).remaining_mut()
    }
}

impl<R: IoBuf + ?Sized> IoBuf for &mut R {
    #[inline]
    fn remaining(&self) -> usize {
        (**self).remaining()
    }
}

impl<W: IoBufMut + ?Sized> IoBufMut for &mut W {
    #[inline]
    fn remaining_mut(&self) -> usize {
        (**self).remaining_mut()
    }
}

#[cfg(feature = "alloc")]
impl<R: IoBuf + ?Sized> IoBuf for Box<R> {
    #[inline]
    fn remaining(&self) -> usize {
        (**self).remaining()
    }
}

#[cfg(feature = "alloc")]
impl<W: IoBufMut + ?Sized> IoBufMut for Box<W> {
    #[inline]
    fn remaining_mut(&self) -> usize {
        (**self).remaining_mut()
    }
}

// =============================================================================
// In-memory buffer implementations

impl IoBuf for [u8] {
    #[inline]
    fn remaining(&self) -> usize {
        self.len()
    }
}

impl IoBufMut for [u8] {
    #[inline]
    fn remaining_mut(&self) -> usize {
        self.len()
    }
}

#[cfg(feature = "alloc")]
impl IoBufMut for Vec<u8> {
    #[inline]
    fn remaining_mut(&self) -> usize {
        // A vector can never have more than isize::MAX bytes
        isize::MAX as usize - self.len()
    }
}

#[cfg(feature = "alloc")]
impl IoBufMut for VecDeque<u8> {
    #[inline]
    fn remaining_mut(&self) -> usize {
        // A vector can never have more than isize::MAX bytes
        isize::MAX as usize - self.len()
    }
}

impl IoBufMut for BorrowedCursor<'_> {
    #[inline]
    fn remaining_mut(&self) -> usize {
        self.capacity()
    }
}
