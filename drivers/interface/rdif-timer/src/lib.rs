#![no_std]

extern crate alloc;

use alloc::boxed::Box;

pub use rdif_base::{DriverGeneric, irq::*};

pub type Hardware = Box<dyn Interface>;
pub type HardwareCPU = Box<dyn InterfaceCPU>;

pub trait Interface: DriverGeneric {
    fn get_current_cpu(&mut self) -> Box<dyn InterfaceCPU>;
}

pub trait InterfaceCPU: Send + Sync {
    fn set_timeval(&self, ticks: u64);
    fn current_ticks(&self) -> u64;
    fn tick_hz(&self) -> u64;
    fn set_irq_enable(&self, enable: bool);
    fn get_irq_status(&self) -> bool;
    fn irq(&self) -> IrqConfig;
}
