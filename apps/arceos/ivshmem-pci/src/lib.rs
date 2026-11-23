#![no_std]

//! ArceOS glue for the AxVisor ivshmem PCI endpoint.
//!
//! The device API is compiled only with the `arceos` feature so that other
//! users of this package do not accidentally acquire ArceOS mappings. Guest
//! protocol state machines and IVC message handling remain outside this
//! low-level device boundary.

#[cfg(feature = "arceos")]
mod arceos;

#[cfg(feature = "arceos")]
pub use arceos::{
    EVENT_VECTOR_0, IvshmemDevice, IvshmemError, IvshmemIdentity, IvshmemLayout, OutputSection,
};
