mod definitions;
mod instructions;
mod percpu;
mod structs;
mod vcpu;
mod vmcs;

use self::structs::VmxBasic;
use axerrno::ax_err_type;

pub use self::definitions::VmxExitReason;
pub use self::percpu::VmxPerCpuState as VmxArchPerCpuState;
pub use self::vcpu::VmxVcpu as VmxArchVCpu;
pub use self::vmcs::{VmxExitInfo, VmxInterruptInfo, VmxIoExitInfo};

/// Return if current platform support virtualization extension.
pub fn has_hardware_support() -> bool {
    if let Some(feature) = raw_cpuid::CpuId::new().get_feature_info() {
        feature.has_vmx()
    } else {
        false
    }
}

pub fn read_vmcs_revision_id() -> u32 {
    VmxBasic::read().revision_id
}

fn as_axerr(err: x86::vmx::VmFail) -> axerrno::AxError {
    use x86::vmx::VmFail;
    match err {
        VmFail::VmFailValid => ax_err_type!(BadState, vmcs::instruction_error().as_str()),
        VmFail::VmFailInvalid => ax_err_type!(BadState, "VMCS pointer is not valid"),
    }
}
