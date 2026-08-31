//! Architecture-neutral contracts shared by target implementations.

pub(crate) mod capabilities;
#[cfg_attr(
    not(any(target_arch = "aarch64", target_arch = "riscv64")),
    expect(
        dead_code,
        reason = "CPU-up is an intentionally absent capability on this target"
    )
)]
pub(crate) mod cpu_up;
pub(crate) mod exit;
pub(crate) mod ops;
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
    BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuEventWait, VcpuRunAction,
};

/// Complete compile-time contract implemented by every selected guest architecture.
///
/// Common VM runtime code depends on this interface. Optional architecture
/// abilities remain separate traits and are implemented only by architectures
/// that actually provide them.
pub(crate) trait Architecture:
    ArchOps + MachinePlatform + GuestBootPlatform + BootImagePlatform
{
    fn run_vcpu(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> crate::AxVmResult<VcpuRunAction>
    where
        Self: Sized,
    {
        ops::run_vcpu::<Self>(vm, vcpu)
    }
}
