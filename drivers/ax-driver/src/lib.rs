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

#[cfg(feature = "pci")]
pub mod pci;
#[cfg(feature = "rknpu")]
pub mod rknpu;
#[cfg(feature = "serial")]
pub mod serial;
#[cfg(any(
    feature = "rockchip-soc",
    feature = "rockchip-pm",
    feature = "sg2002-placeholder",
    feature = "rockchip-dwmmc"
))]
pub mod soc;
#[cfg(all(any(target_arch = "aarch64", target_arch = "riscv64"), plat_dyn))]
pub mod time;
#[cfg(feature = "usb")]
pub mod usb;
#[cfg(virtio_dev)]
pub mod virtio;

pub use binding_info::BindingInfo;
#[cfg(feature = "pci")]
pub use binding_info::PciIrqRequirement;
#[cfg(feature = "pci")]
pub use binding_resolver::binding_info_from_pci;
pub use binding_resolver::{
    binding_info_from_acpi, binding_info_from_acpi_route, binding_info_from_fdt,
};
pub use error::{Error, Result};
