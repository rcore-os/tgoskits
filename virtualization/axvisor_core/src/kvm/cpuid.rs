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

use alloc::vec::Vec;

use ax_errno::{AxResult, ax_err};
use axvisor_api::control as api_control;

use super::{CONTROL_FILES, ControlFileState};
use crate::kvm::{
    abi::raw as abi,
    state::KvmCpuidEntry2,
    util::{checked_add, read_u32_user, write_u32_user},
};

pub(in crate::kvm) fn get_supported_cpuid(arg: usize) -> AxResult<isize> {
    let entries = supported_cpuid_entries();
    write_cpuid_entries(arg, &entries)
}

pub(in crate::kvm) fn set_cpuid2(
    control_file: api_control::ControlFileId,
    arg: usize,
) -> AxResult<isize> {
    let entries = read_cpuid_entries(arg)?;
    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    vcpu.cpuid = entries;
    Ok(0)
}

pub(in crate::kvm) fn get_cpuid2(
    control_file: api_control::ControlFileId,
    arg: usize,
) -> AxResult<isize> {
    let entries = {
        let control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
            return ax_err!(NotFound);
        };
        if vcpu.cpuid.is_empty() {
            supported_cpuid_entries()
        } else {
            vcpu.cpuid.clone()
        }
    };
    write_cpuid_entries(arg, &entries)
}

fn read_cpuid_entries(arg: usize) -> AxResult<Vec<KvmCpuidEntry2>> {
    let nent = read_u32_user(arg)? as usize;
    if nent > abi::KVM_MAX_CPUID_ENTRIES {
        return ax_err!(InvalidInput);
    }
    let entries_offset = checked_add(arg, abi::KVM_CPUID2_SIZE as usize)?;
    let mut entries = Vec::with_capacity(nent);
    for index in 0..nent {
        let offset = checked_add(entries_offset, index * abi::KVM_CPUID_ENTRY2_SIZE)?;
        entries.push(read_cpuid_entry(offset)?);
    }
    Ok(entries)
}

fn write_cpuid_entries(arg: usize, entries: &[KvmCpuidEntry2]) -> AxResult<isize> {
    let requested = read_u32_user(arg)? as usize;
    write_u32_user(arg, entries.len() as u32)?;
    if requested < entries.len() {
        return ax_err!(ArgumentListTooLong);
    }

    let mut offset = checked_add(arg, abi::KVM_CPUID2_SIZE as usize)?;
    for entry in entries {
        api_control::copy_to_user(offset, &entry.to_bytes())?;
        offset = checked_add(offset, abi::KVM_CPUID_ENTRY2_SIZE)?;
    }
    Ok(0)
}

fn read_cpuid_entry(arg: usize) -> AxResult<KvmCpuidEntry2> {
    let mut bytes = [0u8; abi::KVM_CPUID_ENTRY2_SIZE];
    api_control::copy_from_user(arg, &mut bytes)?;

    Ok(KvmCpuidEntry2 {
        function: u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
        index: u32::from_ne_bytes(bytes[4..8].try_into().unwrap()),
        flags: u32::from_ne_bytes(bytes[8..12].try_into().unwrap()),
        eax: u32::from_ne_bytes(bytes[12..16].try_into().unwrap()),
        ebx: u32::from_ne_bytes(bytes[16..20].try_into().unwrap()),
        ecx: u32::from_ne_bytes(bytes[20..24].try_into().unwrap()),
        edx: u32::from_ne_bytes(bytes[24..28].try_into().unwrap()),
    })
}

impl KvmCpuidEntry2 {
    fn to_bytes(self) -> [u8; abi::KVM_CPUID_ENTRY2_SIZE] {
        let mut bytes = [0u8; abi::KVM_CPUID_ENTRY2_SIZE];
        bytes[0..4].copy_from_slice(&self.function.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.index.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.flags.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.eax.to_ne_bytes());
        bytes[16..20].copy_from_slice(&self.ebx.to_ne_bytes());
        bytes[20..24].copy_from_slice(&self.ecx.to_ne_bytes());
        bytes[24..28].copy_from_slice(&self.edx.to_ne_bytes());
        bytes
    }
}

fn supported_cpuid_entries() -> Vec<KvmCpuidEntry2> {
    #[cfg(target_arch = "x86_64")]
    {
        supported_cpuid_entries_x86_64()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        Vec::new()
    }
}

#[cfg(target_arch = "x86_64")]
fn supported_cpuid_entries_x86_64() -> Vec<KvmCpuidEntry2> {
    let max_basic = host_cpuid(0, 0).eax;
    let mut entries = Vec::new();

    push_host_cpuid(&mut entries, 0, 0, 0);
    if max_basic >= 1 {
        push_host_cpuid(&mut entries, 1, 0, 0);
    }
    if max_basic >= 4 {
        for index in 0..=16 {
            let entry = host_cpuid(4, index);
            if entry.eax & 0x1f == 0 {
                break;
            }
            entries.push(KvmCpuidEntry2 {
                function: 4,
                index,
                flags: abi::KVM_CPUID_FLAG_SIGNIFICANT_INDEX,
                ..entry
            });
        }
    }
    if max_basic >= 6 {
        push_host_cpuid(&mut entries, 6, 0, 0);
    }
    if max_basic >= 7 {
        let max_subleaf = host_cpuid(7, 0).eax.min(2);
        for index in 0..=max_subleaf {
            push_host_cpuid(
                &mut entries,
                7,
                index,
                abi::KVM_CPUID_FLAG_SIGNIFICANT_INDEX,
            );
        }
    }
    if max_basic >= 0xa {
        push_host_cpuid(&mut entries, 0xa, 0, 0);
    }
    if max_basic >= 0xb {
        for index in 0..=8 {
            let entry = host_cpuid(0xb, index);
            if index != 0 && entry.ebx == 0 {
                break;
            }
            entries.push(KvmCpuidEntry2 {
                function: 0xb,
                index,
                flags: abi::KVM_CPUID_FLAG_SIGNIFICANT_INDEX,
                ..entry
            });
        }
    }
    if max_basic >= 0xd {
        push_host_cpuid(&mut entries, 0xd, 0, abi::KVM_CPUID_FLAG_SIGNIFICANT_INDEX);
        push_host_cpuid(&mut entries, 0xd, 1, abi::KVM_CPUID_FLAG_SIGNIFICANT_INDEX);
    }
    if max_basic >= 0x15 {
        push_host_cpuid(&mut entries, 0x15, 0, 0);
    }
    if max_basic >= 0x16 {
        push_host_cpuid(&mut entries, 0x16, 0, 0);
    }
    if max_basic >= 0x1f {
        for index in 0..=8 {
            let entry = host_cpuid(0x1f, index);
            if index != 0 && entry.ebx == 0 {
                break;
            }
            entries.push(KvmCpuidEntry2 {
                function: 0x1f,
                index,
                flags: abi::KVM_CPUID_FLAG_SIGNIFICANT_INDEX,
                ..entry
            });
        }
    }

    let max_extended = host_cpuid(0x8000_0000, 0).eax;
    push_host_cpuid(&mut entries, 0x8000_0000, 0, 0);
    for function in 0x8000_0001..=max_extended.min(0x8000_0008) {
        push_host_cpuid(&mut entries, function, 0, 0);
    }

    entries
}

#[cfg(target_arch = "x86_64")]
fn push_host_cpuid(entries: &mut Vec<KvmCpuidEntry2>, function: u32, index: u32, flags: u32) {
    let mut entry = host_cpuid(function, index);
    entry.flags = flags;
    entries.push(entry);
}

#[cfg(target_arch = "x86_64")]
fn host_tsc_frequency_mhz() -> u32 {
    const FALLBACK_TSC_FREQUENCY_MHZ: u32 = 3_000;
    axvisor_api::arch::host_tsc_frequency_mhz().unwrap_or(FALLBACK_TSC_FREQUENCY_MHZ)
}

#[cfg(target_arch = "x86_64")]
pub(in crate::kvm) fn default_tsc_khz() -> u32 {
    host_tsc_frequency_mhz().saturating_mul(1_000)
}

#[cfg(target_arch = "x86_64")]
fn host_cpuid(function: u32, index: u32) -> KvmCpuidEntry2 {
    let result = core::arch::x86_64::__cpuid_count(function, index);
    let mut entry = KvmCpuidEntry2 {
        function,
        index,
        flags: 0,
        eax: result.eax,
        ebx: result.ebx,
        ecx: result.ecx,
        edx: result.edx,
    };

    match function {
        1 => {
            entry.ecx |= 1 << 31; // hypervisor present
            entry.ecx &= !(1 << 5); // VMX
        }
        0x8000_0001 => {
            entry.ecx &= !(1 << 2); // SVM
        }
        _ => {}
    }
    entry
}
