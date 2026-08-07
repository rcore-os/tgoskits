//! Architecture-neutral contracts shared by target implementations.

pub(crate) mod capabilities;
mod exit;
pub(crate) mod ops;
mod types;

pub(crate) use capabilities::{
    BootImagePlatform, GuestBootPlatform, HostTimePlatform, MachinePlatform,
    minimum_recorded_target_cpu_capability, unsupported_target_cpu_capability,
};
pub(crate) use exit::{handle_hypercall, handle_mmio_read, handle_mmio_write};
#[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
pub(crate) use exit::{try_handle_mmio_read, try_handle_mmio_write};
pub(crate) use ops::ArchOps;
pub(crate) use types::{BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuRunAction};
