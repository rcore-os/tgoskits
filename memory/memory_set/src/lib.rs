#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

extern crate alloc;

mod area;
mod backend;
mod set;

#[cfg(test)]
mod tests;

pub use self::{area::MemoryArea, backend::MappingBackend, set::MemorySet};

/// Error type for memory mapping operations.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum MappingError {
    /// Invalid parameter (e.g., `addr`, `size`, `flags`, etc.)
    #[error("invalid mapping parameter")]
    InvalidParam,
    /// The given range overlaps with an existing mapping.
    #[error("mapping already exists")]
    AlreadyExists,
    /// The backend page table is in a bad state.
    #[error("mapping backend is in a bad state")]
    BadState,
}

/// A [`Result`] type with [`MappingError`] as the error type.
pub type MappingResult<T = ()> = Result<T, MappingError>;
