#![no_std]

extern crate alloc;

use rdif_base::def_driver;
pub use rdif_base::{DriverGeneric, KError, custom_type};

custom_type!(
    #[doc = "Clock signal id"],
    ClockId, usize, "{:#x}");

pub trait Interface: DriverGeneric {
    fn perper_enable(&mut self);

    fn get_rate(&self, id: ClockId) -> Result<u64, KError>;

    fn set_rate(&mut self, id: ClockId, rate: u64) -> Result<(), KError>;
}

def_driver!(Clk, Interface);
