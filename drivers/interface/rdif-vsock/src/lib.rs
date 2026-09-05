#![no_std]

extern crate alloc;

mod addr;
mod error;
mod event;
mod interface;
mod irq;

pub use addr::*;
pub use error::*;
pub use event::*;
pub use interface::*;
pub use irq::*;
pub use rdif_base::{DriverGeneric, KError, io};
