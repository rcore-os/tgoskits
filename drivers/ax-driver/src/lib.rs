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
#[cfg(any(feature = "cv181x-sdhci", feature = "aic8800-wifi"))]
mod cv181x;
pub mod error;
mod irq_binding;
pub mod mmio;
#[cfg(any(
    feature = "block",
    feature = "display",
    feature = "audio-playback",
    feature = "input",
    feature = "net",
    feature = "usb",
    feature = "vsock"
))]
mod registration;
#[cfg(any(
    feature = "aic8800-wifi",
    feature = "cv181x-sdhci",
    feature = "k230-sdhci",
    feature = "rockchip-sdhci"
))]
mod sdhci_runtime;

#[cfg(feature = "block")]
pub mod block;
#[cfg(feature = "display")]
pub mod display;
#[cfg(feature = "input")]
pub mod input;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "audio-playback")]
pub mod playback;
#[cfg(feature = "vsock")]
pub mod vsock;

#[cfg(feature = "jpeg")]
pub mod jpeg;
#[cfg(feature = "pci")]
pub mod pci;
#[cfg(feature = "pwm")]
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
    feature = "starfive-soc"
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
pub use irq_binding::IrqBindingLease;

/// Resolves a portable device's DMA metadata to the ArceOS-owned backend.
///
/// Block runtimes must retain the bound backend when they allocate I/O
/// buffers; rebuilding a direct capability from translated metadata would
/// hand a physical address to an IOMMU-managed PCI device.
pub fn dma_device_for_info(
    info: dma_api::DmaDeviceInfo,
) -> core::result::Result<dma_api::DeviceDma, dma_api::DmaError> {
    match info.domain() {
        dma_api::DmaDomainId::Direct => Ok(axklib::dma::device(info)),
        dma_api::DmaDomainId::Translated(_) => {
            #[cfg(feature = "arm-smmu-v3")]
            {
                pci::dma_for_info(info)
            }
            #[cfg(not(feature = "arm-smmu-v3"))]
            {
                Err(dma_api::DmaError::DomainMismatch {
                    requested: info.domain(),
                    backend: dma_api::DmaDomainId::Direct,
                })
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod test_support;
