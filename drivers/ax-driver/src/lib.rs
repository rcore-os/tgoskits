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
mod irq_binding;
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
    feature = "rockchip-dwmmc",
    feature = "starfive-soc"
))]
pub mod soc;
#[cfg(feature = "rtc")]
pub mod time;
#[cfg(feature = "usb")]
pub mod usb;
#[cfg(virtio_dev)]
pub mod virtio;

/// RK3588 CPU DVFS ondemand governor, exposed as a stable, arch-neutral entry
/// the kernel can drive from a periodic task without knowing the SoC specifics.
///
/// The governor's *policy + apply* live in the (arch-specific) cpufreq driver,
/// but its *loop* — sleeping between samples and reading the per-CPU busy
/// counters — cannot live in this crate: ax-driver sits below ax-task/ax-hal in
/// the dependency graph, so spawning a task here would be a cyclic dependency.
/// The kernel therefore owns the loop and calls [`cpufreq::governor_poll`] each
/// tick. When the DVFS feature is off these are no-ops so callers stay generic.
pub mod cpufreq {
    #[cfg(feature = "rk3588-cpufreq")]
    pub use crate::soc::rockchip::cpufreq::{
        calibrate_cluster, calibrate_wanted, governor_period_ms, governor_poll, governor_wanted,
    };

    /// Feature-off stub: no governor, so the kernel never spawns its task.
    #[cfg(not(feature = "rk3588-cpufreq"))]
    pub fn governor_wanted() -> bool {
        false
    }
    /// Feature-off stub.
    #[cfg(not(feature = "rk3588-cpufreq"))]
    pub fn governor_period_ms() -> u64 {
        100
    }
    /// Feature-off stub.
    #[cfg(not(feature = "rk3588-cpufreq"))]
    pub fn governor_poll(_busy: &[u64]) {}
    /// Feature-off stub: no calibration.
    #[cfg(not(feature = "rk3588-cpufreq"))]
    pub fn calibrate_wanted() -> bool {
        false
    }
    /// Feature-off stub.
    #[cfg(not(feature = "rk3588-cpufreq"))]
    pub fn calibrate_cluster(_cluster_idx: usize, _intended_cpu: usize) {}
}

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
