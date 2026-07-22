//! Architecture-neutral contracts shared by target implementations.

pub(crate) mod capabilities;
mod exit;
pub(crate) mod ops;
mod types;

pub(crate) use capabilities::{BootImagePlatform, GuestBootPlatform, HostTimePlatform};
pub(crate) use exit::handle_hypercall;
pub(crate) use ops::ArchOps;
pub(crate) use types::{BoundVcpuExit, HypercallExit, VcpuRunAction, VcpuScheduling};
