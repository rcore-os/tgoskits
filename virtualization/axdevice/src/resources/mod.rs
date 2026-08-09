//! Deterministic resource planning for architecture-owned VM initialization.

mod allocation;
mod claim;
mod error;
mod plan;
mod pool;
mod requirements;
mod resolved;

pub use claim::{ResourceClaimSet, ResourceLease};
pub use error::{ResourceNamespace, ResourcePlanningError};
pub use plan::{VmResourcePlan, VmResourcePlanner};
pub use pool::ResourcePools;
pub use requirements::{
    DevicePlanRequest, DeviceRequirement, DeviceRequirements, MsiResourceRequest, ResourceRequest,
    ResourceSlot,
};
pub use resolved::{ResolvedDeviceResources, ResolvedMsi, ResolvedWiredIrq};
