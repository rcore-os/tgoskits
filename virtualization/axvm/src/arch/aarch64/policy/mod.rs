//! AArch64 VM exit, firmware, interrupt, and timer policy above ax-cpu.

mod exception;
mod exception_utils;
mod host;
mod pcpu;
mod smc;
mod vcpu;

/// Context frame saved on an AArch64 guest exception.
pub use ax_cpu::virtualization::GuestContext as TrapFrame;

pub use self::{
    host::ArmHostIrqGuard,
    pcpu::ArmPerCpu,
    vcpu::{ArmVcpu, ArmVcpuCreateConfig, ArmVcpuSetupConfig},
};

/// Returns whether the current platform supports the virtualization extension.
pub const fn has_hardware_support() -> bool {
    true
}

mod timer;
mod types;
pub use timer::{ArmTimerKind, ArmTimerSnapshot, ArmTimerVmConfig, ArmVcpuTimer};
pub use types::{
    ArmAccessWidth, ArmGicCpuInterfaceRegister, ArmGuestPhysAddr, ArmNestedPagingConfig,
    ArmSysRegAddr, ArmVcpuError, ArmVcpuResult, ArmVmExit,
};
