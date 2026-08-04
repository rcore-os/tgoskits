//! Bitmap allocation error mapping helpers.

use crate::{
    bitmap::BitmapError,
    error::{Errno, Ext4Error},
};

pub(crate) fn map_bitmap_error(err: BitmapError) -> Ext4Error {
    match err {
        BitmapError::IndexOutOfRange => Ext4Error::invalid_input(),
        BitmapError::AlreadyAllocated => Ext4Error::already_exists(),
        BitmapError::AlreadyFree => Ext4Error::from(Errno::ENOENT),
    }
}
