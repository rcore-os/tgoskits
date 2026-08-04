//! AArch64 hardware implementation of the portable vCPU and timer contracts.

mod context_frame;
#[macro_use]
mod exception_utils;
mod exception;
mod host;
mod pcpu;
mod smc;
mod vcpu;

pub use self::{
    host::{ArmHostIrqConfig, ArmHostIrqGuard, ArmHostOps},
    pcpu::ArmPerCpu,
    vcpu::{
        ARM_VCPU_HOST_SP_EL0_OFFSET, ARM_VCPU_HOST_STACK_TOP_OFFSET, ARM_VCPU_TRAP_FRAME_SIZE,
        ArmVcpu, ArmVcpuCreateConfig, ArmVcpuSetupConfig,
    },
};

/// Context frame saved on an AArch64 guest exception.
pub type TrapFrame = context_frame::Aarch64ContextFrame;

/// Returns the maximum guest page-table levels supported by the current CPU.
///
/// A physical-address width of at least 44 bits selects four levels and a
/// smaller width selects three levels.
pub fn max_guest_page_table_levels() -> usize {
    vcpu::max_gpt_level(vcpu::pa_bits())
}

/// Returns the physical-address width reported by the current CPU.
pub fn pa_bits() -> usize {
    vcpu::pa_bits()
}

/// Returns whether the current platform supports the virtualization extension.
pub const fn has_hardware_support() -> bool {
    true
}
