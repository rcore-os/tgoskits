#![no_std]

#[macro_use]
extern crate alloc;

mod encode;
mod fdt;
mod node;
mod prop;

pub use fdt_raw::{FdtError, MemoryRegion, Phandle, RegInfo, Status, data::Reader};

/// A unique identifier for a node in the `Fdt` arena.
pub type NodeId = usize;

pub use encode::{FdtData, FdtEncoder};
pub use fdt::*;
pub use node::view::*;
pub use node::*;
pub use prop::*;
