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
    all(feature = "serial", plat_dyn),
    all(feature = "rtc", plat_dyn),
    all(feature = "rockchip-soc", plat_dyn),
    all(feature = "rockchip-pm", plat_dyn),
    all(feature = "sg2002-placeholder", plat_dyn),
    all(feature = "rockchip-dwmmc", plat_dyn),
    all(feature = "rockchip-sdhci", plat_dyn),
    all(feature = "phytium-mci", plat_dyn),
    all(feature = "rk3588-pcie", plat_dyn),
    all(feature = "rknpu", plat_dyn),
    all(feature = "xhci-mmio", target_os = "none", plat_dyn),
    all(feature = "xhci-pci", target_os = "none"),
    all(virtio_dev, plat_dyn)
))]
pub mod mmio;

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

pub mod pci;
#[cfg(feature = "qperf-metrics")]
pub mod qperf_metrics;
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
#[cfg(feature = "rtc")]
pub mod time;
#[cfg(feature = "usb")]
pub mod usb;
#[cfg(virtio_dev)]
pub mod virtio;

pub use error::{Error, Result};
