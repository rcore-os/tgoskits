//! Static platform driver registration facade for the rdrive + rdif device path.

#![no_std]

pub use ax_drivers::{Error, Result, init_static_drivers, register_driver};
