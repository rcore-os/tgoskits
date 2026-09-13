#![no_std]

extern crate alloc;

mod error;
mod event;
mod id;
mod interface;

pub use error::*;
pub use event::*;
pub use id::*;
pub use interface::*;
pub use rdif_base::{DriverGeneric, KError, io};
