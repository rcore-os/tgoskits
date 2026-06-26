#![no_std]

extern crate alloc;
extern crate ax_driver as _;
extern crate somehal;

#[macro_use]
extern crate ax_plat;
#[allow(unused_imports)]
#[macro_use]
extern crate log;

mod boot;
mod console;
pub mod drivers;
mod generic_timer;
mod init;
#[cfg(feature = "irq")]
mod irq;
mod mem;
mod platform;
mod power;

pub use boot::{boot_stack_bounds, bootargs};
pub use generic_timer::try_init_epoch_offset;

#[cfg(feature = "irq")]
pub fn enable_timer_irq() {
    somehal::timer::irq_enable();
}
#[cfg(feature = "irq")]
pub fn ipi_irq() -> ax_plat::irq::IrqId {
    somehal::irq::ipi_irq()
}
#[cfg(all(feature = "irq", target_arch = "riscv64", feature = "hv"))]
pub use irq::register_virtual_irq_injector;

#[cfg(all(feature = "irq", target_arch = "x86_64"))]
pub fn set_ioapic_gsi_enabled_from_irq(
    gsi: u32,
    enabled: bool,
) -> Result<(), ax_plat::irq::IrqError> {
    somehal::arch::set_ioapic_gsi_enabled_from_irq(gsi, enabled)
}
