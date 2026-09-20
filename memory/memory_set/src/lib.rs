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
    /// A fallible operation changed part of the materialized page table but
    /// could not prove that its inverse restored every preimage.  Callers must
    /// quarantine the address-space range and repair it before reuse.
    #[error("mapping operation requires repair")]
    NeedsRepair,
}

/// A [`Result`] type with [`MappingError`] as the error type.
pub type MappingResult<T = ()> = Result<T, MappingError>;
