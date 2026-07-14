//! Architecture-neutral contracts shared by target implementations.

mod capabilities;
mod exit;
pub(crate) mod ops;
mod types;

pub(crate) use capabilities::{BootImagePlatform, GuestBootPlatform};
pub(crate) use exit::{finish_deferred, handle_hypercall, handle_mmio_read, handle_mmio_write};
pub(crate) use ops::ArchOps;
pub(crate) use types::{
    BoundVcpuExit, CommonDeferredRunWork, HypercallExit, MmioReadExit, MmioWriteExit, VcpuRunAction,
};
