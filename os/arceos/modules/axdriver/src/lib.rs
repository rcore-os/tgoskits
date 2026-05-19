//! Static platform driver registration for the rdrive + rdif device path.

#![no_std]

extern crate alloc;

pub mod error;
mod init;
mod registers;
mod source;

#[cfg(feature = "block")]
pub mod block;
#[cfg(feature = "display")]
pub mod display;
#[cfg(feature = "input")]
pub mod input;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "vsock")]
pub mod vsock;

#[cfg(feature = "bus-pci")]
mod pci;
#[cfg(virtio_dev)]
mod virtio;

pub mod prelude;

pub use error::{Error, Result};
pub use init::init_static_drivers;
