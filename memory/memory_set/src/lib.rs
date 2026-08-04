#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

extern crate alloc;

mod area;
mod backend;
mod set;

#[cfg(all(axtest, feature = "axtest"))]
pub mod axtest;

#[cfg(test)]
mod tests;

pub use self::{area::MemoryArea, backend::MappingBackend, set::MemorySet};

/// Error type for memory mapping operations.
#[derive(Debug, Eq, PartialEq)]
pub enum MappingError {
    /// Invalid parameter (e.g., `addr`, `size`, `flags`, etc.)
    InvalidParam,
    /// The given range overlaps with an existing mapping.
    AlreadyExists,
    /// The backend page table is in a bad state.
    BadState,
}

#[cfg(feature = "ax-errno")]
impl From<MappingError> for ax_errno::AxError {
    fn from(err: MappingError) -> Self {
        match err {
            MappingError::InvalidParam => ax_errno::AxError::InvalidInput,
            MappingError::AlreadyExists => ax_errno::AxError::AlreadyExists,
            MappingError::BadState => ax_errno::AxError::BadState,
        }
    }
}

/// A [`Result`] type with [`MappingError`] as the error type.
pub type MappingResult<T = ()> = Result<T, MappingError>;
