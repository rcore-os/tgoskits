#![no_std]

extern crate alloc;

pub mod descriptor;
pub mod endpoint;
pub mod err;
pub mod host;
pub mod transfer;

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DrMode {
    #[default]
    Host,
    Peripheral,
    Otg,
}

pub use host::hub::Speed;
