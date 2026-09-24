#![no_std]

extern crate alloc;

mod hardware;
mod memory;

pub use hardware::{Smmu, SmmuFault};
pub use memory::{PhysicalMemory, PhysicalRegion};
