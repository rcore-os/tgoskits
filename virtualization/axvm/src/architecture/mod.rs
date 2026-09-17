//! Architecture-neutral contracts shared by target implementations.

pub(crate) mod capabilities;
pub(crate) mod exit;
pub(crate) mod ops;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(crate) mod sysreg;
mod types;

pub(crate) use capabilities::{
    BootImagePlatform, GuestBootPlatform, MachinePlatform, minimum_recorded_target_cpu_capability,
    unsupported_target_cpu_capability,
};
pub(crate) use exit::{handle_hypercall, handle_mmio_read, handle_mmio_write};
#[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
pub(crate) use exit::{try_handle_mmio_read, try_handle_mmio_write};
pub(crate) use ops::ArchOps;
pub(crate) use types::{
    HypercallExit, MmioReadExit, MmioWriteExit, VcpuExitAction, VcpuRunAction, VcpuRunOutcome,
};

/// Complete compile-time contract implemented by every selected guest architecture.
///
/// Common VM runtime code depends on this interface. Optional architecture
/// abilities remain separate traits and are implemented only by architectures
/// that actually provide them.
pub(crate) trait Architecture:
    ArchOps + MachinePlatform + GuestBootPlatform + BootImagePlatform
{
}
