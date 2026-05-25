//! rdrive + rdif host driver registration collection.

#![no_std]
#![feature(used_with_arg)]

extern crate alloc;
#[macro_use]
extern crate rdrive;

module_driver!(
    name: "ax-driver macro placeholder",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[],
);

pub mod error;
#[cfg(any(
    feature = "serial",
    all(feature = "rtc", feature = "fdt"),
    all(feature = "rockchip-soc", feature = "fdt"),
    all(feature = "rockchip-pm", feature = "fdt"),
    all(feature = "rockchip-dwmmc", feature = "fdt"),
    all(feature = "rockchip-sdhci", feature = "fdt"),
    all(feature = "phytium-mci", feature = "fdt"),
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
