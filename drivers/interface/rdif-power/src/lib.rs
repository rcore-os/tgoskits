#![no_std]

extern crate alloc;

use rdif_base::def_driver;
pub use rdif_base::{DriverGeneric, KError};

pub trait Interface: DriverGeneric {
    fn shutdown(&mut self);
}

def_driver!(Power, Interface);
