#![no_std]

#[cfg(all(axtest, feature = "axtest"))]
extern crate alloc;

#[cfg(all(axtest, feature = "axtest"))]
/// Coverage tests for kernel utility helpers.
pub mod axtest;

pub mod address;
pub mod id;
pub mod memory;
mod staticcell;

pub use staticcell::StaticCell;
