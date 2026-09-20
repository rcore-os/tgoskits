//! Bitmap allocation error mapping helpers.

use crate::{bitmap::BitmapError, error::Ext4Error};

pub(crate) fn map_bitmap_error(err: BitmapError) -> Ext4Error {
    match err {
        BitmapError::IndexOutOfRange => Ext4Error::invalid_input(),
        BitmapError::AlreadyAllocated => Ext4Error::already_exists(),
        BitmapError::AlreadyFree => Ext4Error::not_found(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_errors_map_to_ext4_errno() {
        assert_eq!(
            map_bitmap_error(BitmapError::IndexOutOfRange).kind(),
            crate::Ext4ErrorKind::InvalidInput
        );
        assert_eq!(
            map_bitmap_error(BitmapError::AlreadyAllocated).kind(),
            crate::Ext4ErrorKind::AlreadyExists
        );
        assert_eq!(
            map_bitmap_error(BitmapError::AlreadyFree).kind(),
            crate::Ext4ErrorKind::NotFound
        );
    }
}
