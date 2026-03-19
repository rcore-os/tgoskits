use core::fmt;

/// Errors that can occur while initializing per-CPU data areas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InitError {
    /// The base address is null and cannot be used.
    InvalidBase,
    /// The base address is not aligned to 64 bytes.
    UnalignedBase,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBase => write!(f, "invalid per-CPU base address"),
            Self::UnalignedBase => write!(f, "unaligned per-CPU base address"),
        }
    }
}

impl core::error::Error for InitError {}
