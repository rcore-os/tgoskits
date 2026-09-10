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

pub use ax_cpu::virtualization::{VmxExitReason, VmxInterruptionType};
mod vcpu;
mod vmcs;

use x86_vlapic::EmulatedLocalApic;

pub use self::{vcpu::VmxVcpu, vmcs::VmxExitInfo};
use crate::arch::x86_64::policy::{X86HostOps, X86HostPhysAddr};

pub fn x86_apic_access_page_addr<H: X86HostOps>() -> X86HostPhysAddr {
    let addr = EmulatedLocalApic::<H>::virtual_apic_access_addr();
    X86HostPhysAddr::from_usize(addr.as_usize())
}
