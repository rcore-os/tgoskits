use core::fmt;

/// The error type used for allocation operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    /// Invalid size, alignment, or other input parameter.
    InvalidParam,
    /// A global allocator instance has already been initialized.
    AlreadyInitialized,
    /// A region overlaps with an existing managed region.
    MemoryOverlap,
    /// Not enough memory is available to satisfy the request.
    NoMemory,
    /// Attempted to deallocate memory that was not allocated.
    NotAllocated,
    /// The allocator has not been initialized.
    NotInitialized,
    /// The requested address or entity was not found in any managed region.
    NotFound,
}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParam => write!(f, "invalid parameter"),
            Self::AlreadyInitialized => write!(f, "allocator already initialized"),
            Self::MemoryOverlap => write!(f, "memory regions overlap"),
            Self::NoMemory => write!(f, "out of memory"),
            Self::NotAllocated => write!(f, "memory not allocated"),
            Self::NotInitialized => write!(f, "allocator not initialized"),
            Self::NotFound => write!(f, "not found"),
        }
    }
}

/// A [`Result`] alias with [`AllocError`] as the error type.
pub type AllocResult<T = ()> = Result<T, AllocError>;
