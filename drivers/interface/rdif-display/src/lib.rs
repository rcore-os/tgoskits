#![no_std]

extern crate alloc;

mod error;
mod interface;
mod types;

pub use error::*;
pub use interface::*;
pub use rdif_base::{DriverGeneric, KError, io};
pub use types::*;
