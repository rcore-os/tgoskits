//! Unified Linux-style error model for the crate.

mod context;
mod errno;

use core::fmt;

pub use context::ErrorContext;
pub use errno::Errno;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ext4Error {
    pub code: Errno,
    pub context: Option<ErrorContext>,
}

pub type EXT4ER = Ext4Error;
pub type Ext4Result<T> = Result<T, Ext4Error>;

impl Ext4Error {
    pub const fn new(code: Errno) -> Self {
        Self {
            code,
            context: None,
        }
    }

    pub const fn with_context(mut self, context: ErrorContext) -> Self {
        self.context = Some(context);
        self
    }

    pub const fn with_operation(self, op: &'static str) -> Self {
        self.with_context(ErrorContext::Operation { op })
    }

    pub const fn invalid_input() -> Self {
        Self::new(Errno::EINVAL)
    }

    pub const fn not_found() -> Self {
        Self::new(Errno::ENOENT)
    }

    pub const fn already_exists() -> Self {
        Self::new(Errno::EEXIST)
    }

    pub const fn not_dir() -> Self {
        Self::new(Errno::ENOTDIR)
    }

    pub const fn is_dir() -> Self {
        Self::new(Errno::EISDIR)
    }

    pub const fn io() -> Self {
        Self::new(Errno::EIO)
    }

    pub const fn badf() -> Self {
        Self::new(Errno::EBADF)
    }

    pub const fn busy() -> Self {
        Self::new(Errno::EBUSY)
    }

    pub const fn no_space() -> Self {
        Self::new(Errno::ENOSPC)
    }

    pub const fn read_only() -> Self {
        Self::new(Errno::EROFS)
    }

    pub const fn permission_denied() -> Self {
        Self::new(Errno::EACCES)
    }

    pub const fn unsupported() -> Self {
        Self::new(Errno::EOPNOTSUPP)
    }

    pub const fn timeout() -> Self {
        Self::new(Errno::ETIMEDOUT)
    }

    pub const fn corrupted() -> Self {
        Self::new(Errno::EUCLEAN)
    }

    pub const fn checksum() -> Self {
        Self::new(Errno::EUCLEAN)
    }

    pub const fn bad_superblock() -> Self {
        Self::new(Errno::EINVAL)
    }

    pub const fn invalid_magic() -> Self {
        Self::new(Errno::EINVAL)
    }

    pub const fn already_mounted() -> Self {
        Self::new(Errno::EBUSY)
    }

    pub const fn block_out_of_range(block_id: u32, max_blocks: u64) -> Self {
        Self::new(Errno::EINVAL).with_context(ErrorContext::BlockRange {
            block_id,
            max_blocks,
        })
    }

    pub const fn invalid_block_size(size: usize, expected: usize) -> Self {
        Self::new(Errno::EINVAL).with_context(ErrorContext::BlockSize { size, expected })
    }

    pub const fn buffer_too_small(provided: usize, required: usize) -> Self {
        Self::new(Errno::EINVAL).with_context(ErrorContext::BufferSize { provided, required })
    }

    pub const fn alignment(offset: u64, alignment: u32) -> Self {
        Self::new(Errno::EINVAL).with_context(ErrorContext::Alignment { offset, alignment })
    }
}

impl From<Errno> for Ext4Error {
    fn from(code: Errno) -> Self {
        Self::new(code)
    }
}

impl fmt::Display for Ext4Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.context {
            Some(context) => write!(
                f,
                "{}: {} [{}]",
                self.code.name(),
                self.code.description(),
                context
            ),
            None => write!(f, "{}: {}", self.code.name(), self.code.description()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::string::ToString;

    #[test]
    fn errno_values_match_linux() {
        assert_eq!(Errno::EPERM.as_i32(), 1);
        assert_eq!(Errno::EIO.as_i32(), 5);
        assert_eq!(Errno::EINVAL.as_i32(), 22);
        assert_eq!(Errno::ENOENT.as_i32(), 2);
        assert_eq!(Errno::EEXIST.as_i32(), 17);
        assert_eq!(Errno::ENOSPC.as_i32(), 28);
        assert_eq!(Errno::EROFS.as_i32(), 30);
        assert_eq!(Errno::EOPNOTSUPP.as_i32(), 95);
        assert_eq!(Errno::EUCLEAN.as_i32(), 117);
        assert_eq!(Errno::EWOULDBLOCK.as_i32(), Errno::EAGAIN.as_i32());
    }

    #[test]
    fn ext4_error_display_keeps_context() {
        let err = Ext4Error::buffer_too_small(4, 8);
        assert_eq!(err.code, Errno::EINVAL);
        assert!(err.to_string().contains("provided=4"));
    }
}
