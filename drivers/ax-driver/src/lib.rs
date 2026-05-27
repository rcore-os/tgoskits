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

crate::model_register!(
    name: "ax-driver macro placeholder",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[],
);

pub mod error;
#[cfg(any(
    all(target_os = "none", feature = "serial"),
    all(target_os = "none", feature = "rtc"),
    feature = "rockchip-soc",
    feature = "rockchip-pm",
    feature = "rockchip-dwmmc",
    feature = "rockchip-sdhci",
    feature = "phytium-mci",
    feature = "rk3588-pcie",
    feature = "rknpu",
    feature = "xhci-mmio",
    feature = "xhci-pci",
    virtio_dev,
))]
pub mod mmio;

#[cfg(any(
    feature = "ahci",
    feature = "bcm2835-sdhci",
    feature = "nvme",
    feature = "ramdisk",
    feature = "virtio-blk",
    feature = "phytium-mci",
    feature = "rockchip-dwmmc",
    feature = "rockchip-sdhci",
))]
pub mod block;
#[cfg(feature = "display")]
pub mod display;
#[cfg(feature = "input")]
pub mod input;
#[cfg(any(
    feature = "fxmac",
    feature = "intel-net",
    feature = "ixgbe",
    feature = "realtek-rtl8125",
    feature = "virtio-net",
))]
pub mod net;
#[cfg(feature = "vsock")]
pub mod vsock;

#[cfg(any(
    target_os = "none",
    feature = "ahci",
    feature = "fxmac",
    feature = "ixgbe",
    feature = "intel-net",
    feature = "realtek-rtl8125",
    feature = "nvme",
    feature = "xhci-pci",
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket",
    feature = "pci-list-devices",
    feature = "rk3588-pcie",
))]
pub mod pci;
#[cfg(feature = "rknpu")]
pub mod rknpu;
#[cfg(all(target_os = "none", feature = "serial"))]
pub mod serial;
#[cfg(any(
    feature = "rockchip-soc",
    feature = "rockchip-pm",
    feature = "rockchip-dwmmc"
))]
pub mod soc;
#[cfg(all(target_os = "none", feature = "rtc"))]
pub mod time;
#[cfg(any(
    feature = "rockchip-dwc-xhci",
    feature = "xhci-mmio",
    feature = "xhci-pci",
))]
pub mod usb;
#[cfg(virtio_dev)]
pub mod virtio;

pub use error::{Error, Result};
