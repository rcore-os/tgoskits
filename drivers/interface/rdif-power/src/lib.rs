#![no_std]

extern crate alloc;

use rdif_base::def_driver;
pub use rdif_base::{DriverGeneric, custom_type};

custom_type!(
    #[doc = "Power domain id"],
    PowerDomainId, u64, "{:#x}");

impl PowerDomainId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

impl From<u32> for PowerDomainId {
    fn from(value: u32) -> Self {
        Self(u64::from(value))
    }
}

impl From<usize> for PowerDomainId {
    fn from(value: usize) -> Self {
        Self(value as u64)
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerError {
    #[error("invalid power domain id")]
    InvalidId,
    #[error("unsupported power operation")]
    Unsupported,
    #[error("power controller is busy")]
    Busy,
    #[error("power controller error")]
    Controller,
}

pub trait Interface: DriverGeneric {
    fn power_on(&mut self, id: PowerDomainId) -> Result<(), PowerError>;

    fn power_off(&mut self, id: PowerDomainId) -> Result<(), PowerError>;

    fn is_powered(&self, _id: PowerDomainId) -> Result<bool, PowerError> {
        Err(PowerError::Unsupported)
    }
}

def_driver!(Power, Interface);
