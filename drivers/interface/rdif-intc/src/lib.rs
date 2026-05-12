#![no_std]

extern crate alloc;

pub use rdif_base::_rdif_prelude::*;
use rdif_base::def_driver;

pub trait Interface: DriverGeneric {
    fn setup_irq_by_fdt(&mut self, _irq_prop: &[u32]) -> IrqId {
        unimplemented!();
    }
}

def_driver!(Intc, Interface);
