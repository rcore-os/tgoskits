//! Error types for bitmap operations.

/// Errors returned by bitmap mutation helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitmapError {
    /// The requested bit index is outside the bitmap range.
    IndexOutOfRange,
    /// The target bit is already allocated.
    AlreadyAllocated,
    /// The target bit is already free.
    AlreadyFree,
}

impl core::fmt::Display for BitmapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BitmapError::IndexOutOfRange => write!(f, "bitmap index out of range"),
            BitmapError::AlreadyAllocated => write!(f, "bitmap entry is already allocated"),
            BitmapError::AlreadyFree => write!(f, "bitmap entry is already free"),
        }
    }
}
