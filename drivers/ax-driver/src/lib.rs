//! rdrive + rdif host driver registration collection.

#![no_std]
#![feature(used_with_arg)]

extern crate alloc;

pub use rdrive::{DriverGeneric, IrqId, KError, PlatformDevice, ProbeError, probe, register};
#[doc(hidden)]
pub use rdrive_macros::__mod_maker;

#[macro_export]
macro_rules! model_register {
    (
        $($i:ident : $t:expr),+,
    ) => {
        $crate::__mod_maker! {
            pub mod some {
                #[allow(unused_imports)]
                use super::*;
                use $crate::register::*;

                /// Static instance of driver registration information.
                ///
                /// This static variable is placed in the `.driver.register` linker section
                /// so that the driver manager can automatically discover and load it during
                /// system startup.
                #[unsafe(link_section = ".driver.register")]
                #[unsafe(no_mangle)]
                #[used(linker)]
                pub static DRIVER: DriverRegister = DriverRegister {
                    $($i : $t),+
                };
            }
        }
    };
}

model_register!(
    name: "ax-driver macro placeholder",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[],
);

mod binding_info;
mod binding_resolver;
pub mod error;
pub mod mmio;
#[cfg(any(
    feature = "block",
    feature = "display",
    feature = "input",
    feature = "net",
    feature = "usb",
    feature = "vsock"
))]
mod registration;

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

#[cfg(feature = "jpeg")]
pub mod jpeg;
#[cfg(feature = "pci")]
pub mod pci;
#[cfg(feature = "rk3588-pwm")]
pub mod pwm;
#[cfg(feature = "rga")]
pub mod rga;
#[cfg(feature = "rknpu")]
pub mod rknpu;
#[cfg(feature = "serial")]
pub mod serial;
#[cfg(any(
    feature = "rockchip-soc",
    feature = "rockchip-pm",
    feature = "rockchip-dwmmc"
))]
pub mod soc;
#[cfg(feature = "rtc")]
pub mod time;
#[cfg(feature = "usb")]
pub mod usb;
#[cfg(virtio_dev)]
pub mod virtio;

#[cfg(feature = "pci")]
pub use binding_info::PciIrqRequirement;
pub use binding_info::{BindingInfo, BindingIrq, BindingIrqBinding, BindingIrqSource, FdtIrqSpec};
#[cfg(feature = "pci")]
pub use binding_resolver::binding_info_from_pci;
pub use binding_resolver::{
    binding_info_from_acpi, binding_info_from_acpi_route, binding_info_from_fdt,
    binding_irq_from_named_fdt_interrupt,
};
pub use error::{Error, Result};
