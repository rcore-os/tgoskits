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

//! Synthetic CPUID leaves exposed by KVM-compatible x86 guests.

use raw_cpuid::CpuIdResult;

pub const KVM_HYPERVISOR_INFO_LEAF: u32 = 0x4000_0000;
pub const KVM_HYPERVISOR_FEATURE_LEAF: u32 = 0x4000_0001;

const KVM_CLOCKSOURCE2_FEATURE: u32 = 1 << 3;
const KVM_VENDOR_REGS: [u32; 3] = [
    u32::from_le_bytes(*b"KVMK"),
    u32::from_le_bytes(*b"VMKV"),
    u32::from_le_bytes(*b"M\0\0\0"),
];

/// Returns the CPUID leaf values used by Linux KVM's paravirtual interface.
pub fn kvm_hypervisor_cpuid(function: u32) -> Option<CpuIdResult> {
    match function {
        KVM_HYPERVISOR_INFO_LEAF => Some(CpuIdResult {
            eax: KVM_HYPERVISOR_FEATURE_LEAF,
            ebx: KVM_VENDOR_REGS[0],
            ecx: KVM_VENDOR_REGS[1],
            edx: KVM_VENDOR_REGS[2],
        }),
        KVM_HYPERVISOR_FEATURE_LEAF => Some(CpuIdResult {
            eax: KVM_CLOCKSOURCE2_FEATURE,
            ebx: 0,
            ecx: 0,
            edx: 0,
        }),
        _ => None,
    }
}

const RUSTVISOR_VENDOR_REGS: [u32; 3] = [
    u32::from_le_bytes(*b"RVMR"),
    u32::from_le_bytes(*b"VMRV"),
    u32::from_le_bytes(*b"MRVM"),
];

/// Returns the Rustvisor vendor leaves used by existing x86_vcpu guests.
pub fn rustvisor_hypervisor_cpuid(function: u32) -> Option<CpuIdResult> {
    match function {
        KVM_HYPERVISOR_INFO_LEAF => Some(CpuIdResult {
            eax: KVM_HYPERVISOR_FEATURE_LEAF,
            ebx: RUSTVISOR_VENDOR_REGS[0],
            ecx: RUSTVISOR_VENDOR_REGS[1],
            edx: RUSTVISOR_VENDOR_REGS[2],
        }),
        KVM_HYPERVISOR_FEATURE_LEAF => Some(CpuIdResult {
            eax: 0,
            ebx: 0,
            ecx: 0,
            edx: 0,
        }),
        _ => None,
    }
}
