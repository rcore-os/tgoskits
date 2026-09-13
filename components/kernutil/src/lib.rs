#![no_std]

#[cfg(test)]
extern crate alloc;

pub mod address;
pub mod id;
pub mod memory;
mod staticcell;

pub use staticcell::StaticCell;
