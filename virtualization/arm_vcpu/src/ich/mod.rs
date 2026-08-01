//! GIC virtualization-interface capability and vCPU execution state.

mod capability;

#[cfg(target_arch = "aarch64")]
pub(crate) use capability::publish_ich_capability;
pub use capability::{
    IchCapabilityError, IchCapabilityProfile, common_ich_capability, ich_capability,
};
