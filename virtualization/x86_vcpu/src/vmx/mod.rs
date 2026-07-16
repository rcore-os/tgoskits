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

use x86_vlapic::EmulatedLocalApic;

use self::structs::VmxBasic;
pub use self::{
    percpu::VmxPerCpuState,
    vcpu::{VmxVcpu, X86_APIC_ACCESS_GPA},
    vmcs::VmxExitInfo,
};
use crate::{X86HostOps, X86HostPhysAddr, X86VcpuError};

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

pub fn x86_apic_access_page_addr<H: X86HostOps>() -> X86HostPhysAddr {
    let addr = EmulatedLocalApic::<H>::virtual_apic_access_addr();
    X86HostPhysAddr::from_usize(addr.as_usize())
}

fn as_axerr(err: x86::vmx::VmFail) -> X86VcpuError {
    use x86::vmx::VmFail;
    match err {
        VmFail::VmFailValid => X86VcpuError::BadState,
        VmFail::VmFailInvalid => X86VcpuError::BadState,
    }
}
