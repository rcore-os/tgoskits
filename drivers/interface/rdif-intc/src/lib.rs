#![no_std]

extern crate alloc;

pub use rdif_base::_rdif_prelude::*;
use rdif_base::def_driver;

pub trait Interface: DriverGeneric {
    fn setup_irq_by_fdt(&mut self, _irq_prop: &[u32]) -> IrqId {
        unimplemented!();
    }

    fn supports_acpi_gsi(&self, _route: &AcpiGsiRoute) -> bool {
        false
    }

    fn setup_irq_by_acpi(&mut self, _route: &AcpiGsiRoute) -> IrqId {
        unimplemented!();
    }
}

def_driver!(intc, Intc, Interface);
