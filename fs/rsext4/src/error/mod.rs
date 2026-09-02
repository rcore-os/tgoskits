//! OS-independent ext4 domain errors.

mod context;

pub use context::{ErrorContext, FeatureSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Ext4ErrorKind {
    #[error("I/O failure")]
    Io,
    #[error("filesystem metadata is corrupted")]
    Corrupted,
    #[error("metadata checksum mismatch")]
    ChecksumMismatch,
    #[error("unsupported filesystem feature")]
    UnsupportedFeature,
    #[error("required runtime capability is unavailable")]
    UnsupportedCapability,
    #[error("operation is unsupported")]
    Unsupported,
    #[error("filesystem is read-only")]
    ReadOnly,
    #[error("no space is available")]
    NoSpace,
    #[error("memory allocation failed")]
    NoMemory,
    #[error("quota limit exceeded")]
    QuotaExceeded,
    #[error("link count limit exceeded")]
    TooManyLinks,
    #[error("journal is aborted")]
    JournalAborted,
    #[error("permission precondition was not satisfied")]
    PermissionDenied,
    #[error("entry was not found")]
    NotFound,
    #[error("entry already exists")]
    AlreadyExists,
    #[error("not a directory")]
    NotDirectory,
    #[error("is a directory")]
    IsDirectory,
    #[error("directory is not empty")]
    NotEmpty,
    #[error("resource is busy")]
    Busy,
    #[error("bad file descriptor")]
    BadFileDescriptor,
    #[error("invalid input")]
    InvalidInput,
    #[error("integer overflow")]
    Overflow,
    #[error("file is too large")]
    FileTooLarge,
    #[error("operation timed out")]
    Timeout,
    #[error("superblock geometry is invalid")]
    BadSuperblock,
    #[error("superblock magic is invalid")]
    InvalidMagic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{kind}: {context:?}")]
pub struct Ext4Error {
    kind: Ext4ErrorKind,
    context: Option<ErrorContext>,
}

pub type Ext4Result<T> = Result<T, Ext4Error>;

impl Ext4Error {
    pub const fn new(kind: Ext4ErrorKind) -> Self {
        Self {
            kind,
            context: None,
        }
    }

    pub const fn kind(self) -> Ext4ErrorKind {
        self.kind
    }

    pub const fn context(self) -> Option<ErrorContext> {
        self.context
    }

    pub const fn is_corruption(self) -> bool {
        matches!(
            self.kind,
            Ext4ErrorKind::Corrupted | Ext4ErrorKind::ChecksumMismatch
        )
    }

    pub const fn with_context(mut self, context: ErrorContext) -> Self {
        self.context = Some(context);
        self
    }

    pub const fn with_operation(self, op: &'static str) -> Self {
        self.with_context(ErrorContext::Operation { op })
    }

    pub const fn invalid_input() -> Self {
        Self::new(Ext4ErrorKind::InvalidInput)
    }

    pub const fn not_found() -> Self {
        Self::new(Ext4ErrorKind::NotFound)
    }

    pub const fn already_exists() -> Self {
        Self::new(Ext4ErrorKind::AlreadyExists)
    }

    pub const fn not_dir() -> Self {
        Self::new(Ext4ErrorKind::NotDirectory)
    }

    pub const fn is_dir() -> Self {
        Self::new(Ext4ErrorKind::IsDirectory)
    }

    pub const fn io() -> Self {
        Self::new(Ext4ErrorKind::Io)
    }

    pub const fn badf() -> Self {
        Self::new(Ext4ErrorKind::BadFileDescriptor)
    }

    pub const fn busy() -> Self {
        Self::new(Ext4ErrorKind::Busy)
    }

    pub const fn not_empty() -> Self {
        Self::new(Ext4ErrorKind::NotEmpty)
    }

    pub const fn no_space() -> Self {
        Self::new(Ext4ErrorKind::NoSpace)
    }

    pub const fn no_memory() -> Self {
        Self::new(Ext4ErrorKind::NoMemory)
    }

    pub const fn too_many_links() -> Self {
        Self::new(Ext4ErrorKind::TooManyLinks)
    }

    pub const fn read_only() -> Self {
        Self::new(Ext4ErrorKind::ReadOnly)
    }

    pub const fn permission_denied() -> Self {
        Self::new(Ext4ErrorKind::PermissionDenied)
    }

    pub const fn unsupported() -> Self {
        Self::new(Ext4ErrorKind::Unsupported)
    }

    pub const fn unsupported_feature(set: FeatureSet, bits: u32) -> Self {
        Self::new(Ext4ErrorKind::UnsupportedFeature)
            .with_context(ErrorContext::Feature { set, bits })
    }

    pub const fn unsupported_capability(name: &'static str) -> Self {
        Self::new(Ext4ErrorKind::UnsupportedCapability)
            .with_context(ErrorContext::Capability { name })
    }

    pub const fn timeout() -> Self {
        Self::new(Ext4ErrorKind::Timeout)
    }

    pub const fn corrupted() -> Self {
        Self::new(Ext4ErrorKind::Corrupted)
    }

    pub const fn checksum() -> Self {
        Self::new(Ext4ErrorKind::ChecksumMismatch)
    }

    pub const fn bad_superblock() -> Self {
        Self::new(Ext4ErrorKind::BadSuperblock)
    }

    pub const fn invalid_magic() -> Self {
        Self::new(Ext4ErrorKind::InvalidMagic)
    }

    pub const fn already_mounted() -> Self {
        Self::new(Ext4ErrorKind::Busy)
    }

    pub const fn overflow() -> Self {
        Self::new(Ext4ErrorKind::Overflow)
    }

    pub const fn file_too_large() -> Self {
        Self::new(Ext4ErrorKind::FileTooLarge)
    }

    pub const fn journal_aborted() -> Self {
        Self::new(Ext4ErrorKind::JournalAborted)
    }

    pub const fn block_out_of_range(block_id: u32, max_blocks: u64) -> Self {
        Self::invalid_input().with_context(ErrorContext::BlockRange {
            block_id,
            max_blocks,
        })
    }

    pub const fn invalid_block_size(size: usize, expected: usize) -> Self {
        Self::invalid_input().with_context(ErrorContext::BlockSize { size, expected })
    }

    pub const fn buffer_too_small(provided: usize, required: usize) -> Self {
        Self::invalid_input().with_context(ErrorContext::BufferSize { provided, required })
    }

    pub const fn alignment(offset: u64, alignment: u32) -> Self {
        Self::invalid_input().with_context(ErrorContext::Alignment { offset, alignment })
    }
}

impl From<Ext4ErrorKind> for Ext4Error {
    fn from(kind: Ext4ErrorKind) -> Self {
        Self::new(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::string::ToString;

    #[test]
    fn domain_error_keeps_kind_and_context() {
        let err = Ext4Error::buffer_too_small(4, 8);
        assert_eq!(err.kind(), Ext4ErrorKind::InvalidInput);
        assert!(err.to_string().contains("provided: 4"));
    }

    #[test]
    fn unsupported_feature_identifies_feature_set_and_bits() {
        let error = Ext4Error::unsupported_feature(FeatureSet::Incompatible, 0x8000_0000);
        assert_eq!(error.kind(), Ext4ErrorKind::UnsupportedFeature);
        assert_eq!(
            error.context(),
            Some(ErrorContext::Feature {
                set: FeatureSet::Incompatible,
                bits: 0x8000_0000,
            })
        );
    }

    #[test]
    fn allocation_failure_has_a_distinct_domain_error() {
        let error = Ext4Error::no_memory();
        assert_eq!(error.kind(), Ext4ErrorKind::NoMemory);
        assert_eq!(error.to_string(), "memory allocation failed: None");
    }
}
