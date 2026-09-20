use ax_memory_set::MappingError;

/// Errors produced by address-space and kernel-mapping operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MmError {
    /// An input address, size, alignment, or range is invalid.
    #[error("invalid memory-management input: {0}")]
    InvalidInput(&'static str),
    /// A page or virtual-address range could not be allocated.
    #[error("memory allocation failed")]
    NoMemory,
    /// The requested virtual mapping overlaps an existing mapping.
    #[error("memory mapping already exists")]
    AlreadyExists,
    /// The requested virtual address is not mapped.
    #[error("bad memory address")]
    BadAddress,
    /// The page table or mapping backend is internally inconsistent.
    #[error("invalid memory-management state: {0}")]
    BadState(&'static str),
    /// The platform cannot provide the requested mapping operation.
    #[error("memory-management operation is unsupported")]
    Unsupported,
    /// A pending stage-1 TLB quarantine could not be confirmed before a new
    /// mutation began. The requested mutation has not started.
    #[error("pending stage-1 TLB quarantine blocked the mutation: {0}")]
    TlbShootdown(ax_hal::cache::TlbShootdownError),
}

impl From<MappingError> for MmError {
    fn from(err: MappingError) -> Self {
        match err {
            MappingError::InvalidParam => Self::InvalidInput("mapping parameters"),
            MappingError::AlreadyExists => Self::AlreadyExists,
            MappingError::BadState => Self::BadState("mapping backend"),
            MappingError::NeedsRepair => Self::BadState("mapping backend requires repair"),
        }
    }
}

/// A memory-management result.
pub type MmResult<T = ()> = Result<T, MmError>;
