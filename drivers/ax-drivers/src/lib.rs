//! rdrive + rdif host driver registration collection.

#![no_std]

extern crate alloc;

pub mod bindings;
pub mod error;
#[cfg(any(
    feature = "serial",
    all(feature = "rtc", feature = "fdt"),
    all(feature = "rockchip-soc", feature = "fdt"),
    all(feature = "rockchip-pm", feature = "fdt"),
    all(feature = "rockchip-dwmmc", feature = "fdt"),
    all(feature = "rockchip-sdhci", feature = "fdt"),
    all(feature = "rk3588-pcie", feature = "fdt"),
    all(feature = "rknpu", feature = "fdt"),
    all(feature = "xhci-mmio", target_os = "none"),
    all(feature = "xhci-pci", target_os = "none"),
    all(virtio_dev, probe = "fdt")
))]
pub mod mmio;

#[cfg(feature = "block")]
pub mod block;
#[cfg(feature = "display")]
pub mod display {}
#[cfg(feature = "input")]
pub mod input {}
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "vsock")]
pub mod vsock {}

#[cfg(feature = "pci")]
pub mod pci;
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

#[macro_export]
macro_rules! register_driver {
    (
        $($i:ident : $t:expr),+,
    ) => {
        rdrive::__mod_maker!{
            pub mod some {
                use super::*;
                use rdrive::register::*;

                #[unsafe(link_section = ".driver.register")]
                #[unsafe(no_mangle)]
                #[used]
                pub static DRIVER: DriverRegister = DriverRegister {
                    $($i : $t),+
                };
            }
        }
    };
}

pub use error::{Error, Result};
