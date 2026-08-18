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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_errors_map_to_ext4_errno() {
        assert_eq!(
            map_bitmap_error(BitmapError::IndexOutOfRange).code,
            Errno::EINVAL
        );
        assert_eq!(
            map_bitmap_error(BitmapError::AlreadyAllocated).code,
            Errno::EEXIST
        );
        assert_eq!(
            map_bitmap_error(BitmapError::AlreadyFree).code,
            Errno::ENOENT
        );
    }
}
