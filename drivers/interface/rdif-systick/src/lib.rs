#![no_std]

extern crate alloc;

use alloc::boxed::Box;

use rdif_base::def_driver;
pub use rdif_base::{DriverGeneric, KError, irq::*};

pub trait Interface: DriverGeneric {
    fn cpu_local(&mut self) -> local::Boxed;
}

pub mod local {
    use super::*;

    pub type Boxed = Box<dyn Interface>;

    pub trait Interface: Send + Sync {
        fn set_timeval(&self, ticks: usize);
        fn current_ticks(&self) -> usize;
        fn tick_hz(&self) -> usize;
        fn set_irq_enable(&self, enable: bool);
        fn get_irq_status(&self) -> bool;
        fn irq(&self) -> IrqConfig;
    }
}

def_driver!(Systick, Interface);
