//! Architecture-neutral contracts shared by target implementations.

mod capabilities;
mod exit;
pub(crate) mod ops;
mod types;

pub(crate) use capabilities::{
    BootImagePlatform, GuestBootPlatform, HostTimePlatform, VmTimerIntegration,
};
pub(crate) use exit::{handle_hypercall, handle_mmio_read, handle_mmio_write};
pub(crate) use ops::ArchOps;
pub(crate) use types::{BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuRunAction};
