// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod definitions;
mod instructions;
mod percpu;
mod structs;
mod vcpu;
mod vmcs;

use ax_errno::ax_err_type;
use axaddrspace::HostPhysAddr;
use x86::vmx::vmcs::control::{PrimaryControls, SecondaryControls};
use x86_vlapic::EmulatedLocalApic;

use self::structs::VmxBasic;
pub use self::{
    definitions::VmxExitReason,
    percpu::VmxPerCpuState as VmxArchPerCpuState,
    vcpu::{VmxVcpu as VmxArchVCpu, X86_APIC_ACCESS_GPA},
    vmcs::{VmxExitInfo, VmxInterruptInfo, VmxIoExitInfo},
};
use crate::msr::Msr;

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

pub fn x86_apic_access_page_addr() -> HostPhysAddr {
    EmulatedLocalApic::virtual_apic_access_addr()
}

pub fn supports_apicv() -> bool {
    let primary_allowed1 = (Msr::IA32_VMX_TRUE_PROCBASED_CTLS.read() >> 32) as u32;
    let secondary_allowed1 = (Msr::IA32_VMX_PROCBASED_CTLS2.read() >> 32) as u32;

    (primary_allowed1 & PrimaryControls::USE_TPR_SHADOW.bits()) != 0
        && (secondary_allowed1 & SecondaryControls::VIRTUALIZE_APIC.bits()) != 0
}

fn as_axerr(err: x86::vmx::VmFail) -> ax_errno::AxError {
    use x86::vmx::VmFail;
    match err {
        VmFail::VmFailValid => ax_err_type!(BadState, vmcs::instruction_error().as_str()),
        VmFail::VmFailInvalid => ax_err_type!(BadState, "VMCS pointer is not valid"),
    }
}
