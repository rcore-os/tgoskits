//! GIC virtualization-interface capability and vCPU execution state.

mod capability;
#[cfg(any(target_arch = "aarch64", test))]
mod context;

#[cfg(target_arch = "aarch64")]
pub(crate) use capability::ich_capability;
pub use capability::{IchCapabilityError, IchCapabilityProfile};
#[cfg(target_arch = "aarch64")]
pub(crate) use capability::{discover_ich_capability, publish_ich_capability};
#[cfg(target_arch = "aarch64")]
pub(crate) use context::{BoundIch, HardwareIchRegisters, IchHcrUpdate, IchVcpuContext};
