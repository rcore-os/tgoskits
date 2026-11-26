//! Input borrowed only while a prepared write performs data I/O.

use crate::Ext4Result;

/// Supplies the bytes for the complete range of one prepared inode write.
///
/// Copy failures preserve their cause and prevent length publication and
/// extent initialization. Earlier physical writes are not a successful logical
/// prefix. Copying runs without a mounted-filesystem borrow; the caller must
/// also release any physical guard that the source can reenter.
pub enum FileWriteInput<'a> {
    /// Immutable resident bytes, submitted directly for full-block writes.
    /// Their length must match the range supplied during preparation.
    Resident(&'a [u8]),
    /// Copies exactly the requested range into bounded kernel staging.
    /// The offset is relative to the complete input. Externally mutable memory
    /// must use this variant instead of being exposed as a shared Rust slice.
    /// An error may initialize part of the output; that chunk is not submitted.
    Copy(&'a mut dyn FnMut(usize, &mut [u8]) -> Ext4Result<()>),
}
