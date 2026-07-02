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

#![no_std]
#![doc = include_str!("../README.md")]

#[cfg(any(feature = "vmx", feature = "svm"))]
#[macro_use]
extern crate log;
#[cfg(not(any(feature = "vmx", feature = "svm")))]
extern crate log;

extern crate alloc;

#[cfg(all(feature = "vmx", feature = "svm"))]
compile_error!("features `vmx` and `svm` are mutually exclusive");

#[cfg(test)]
mod test_utils;

/// x86 vCPU setup configuration.
#[derive(Clone, Copy, Debug, Default)]
pub struct X86VCpuSetupConfig {
    /// Intercept COM1 PIO ports so the VMM path can emulate or forward them.
    pub intercept_com1: bool,
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) mod kvm;
pub(crate) mod msr;
#[cfg(feature = "vmx")]
#[macro_use]
pub(crate) mod regs;
mod ept;
#[cfg(not(feature = "vmx"))]
pub(crate) mod regs;
#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) mod xstate;

#[cfg(any(feature = "vmx", feature = "svm", test))]
const X86_RESET_VECTOR_GPA: usize = 0xffff_fff0;
#[cfg(any(feature = "vmx", feature = "svm", test))]
const X86_RESET_CS_SELECTOR: u16 = 0xf000;
#[cfg(any(feature = "vmx", feature = "svm", test))]
const X86_RESET_CS_BASE: usize = 0xffff_0000;

#[cfg(any(feature = "vmx", feature = "svm", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct X86RealModeEntryState {
    pub(crate) cs_selector: u16,
    pub(crate) cs_base: usize,
    pub(crate) rip: usize,
}

#[cfg(any(feature = "vmx", feature = "svm", test))]
pub(crate) fn x86_real_mode_entry_state(
    entry: axaddrspace::GuestPhysAddr,
) -> X86RealModeEntryState {
    if entry.as_usize() == X86_RESET_VECTOR_GPA {
        return X86RealModeEntryState {
            cs_selector: X86_RESET_CS_SELECTOR,
            cs_base: X86_RESET_CS_BASE,
            rip: X86_RESET_VECTOR_GPA - X86_RESET_CS_BASE,
        };
    }

    X86RealModeEntryState {
        cs_selector: 0,
        cs_base: 0,
        rip: entry.as_usize(),
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "vmx")] {
        mod vmx;
        use vmx as vendor;
        pub use vmx::{VmxExitInfo, VmxExitReason, VmxInterruptInfo, VmxIoExitInfo};

        pub use vendor::{
            VmxArchPerCpuState, VmxArchPerCpuState as X86ArchPerCpuState, VmxArchVCpu,
            VmxArchVCpu as X86ArchVCpu, X86_APIC_ACCESS_GPA, supports_apicv,
            x86_apic_access_page_addr,
        };
    } else if #[cfg(feature = "svm")] {
        mod svm;
        use svm as vendor;

        pub use svm::{SvmExitCode, SvmExitInfo, SvmIntercept};
        pub use vendor::{
            SvmArchPerCpuState, SvmArchPerCpuState as X86ArchPerCpuState, SvmArchVCpu,
            SvmArchVCpu as X86ArchVCpu,
        };
    }
}

pub use ept::GuestPageWalkInfo;
pub use regs::GeneralRegisters;
#[cfg(any(feature = "vmx", feature = "svm"))]
pub use vendor::has_hardware_support;

#[cfg(not(any(feature = "vmx", feature = "svm")))]
pub fn has_hardware_support() -> bool {
    false
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) fn restore_host_interrupt_flag(host_rflags: u64) {
    if host_rflags & x86_64::registers::rflags::RFlags::INTERRUPT_FLAG.bits() != 0 {
        x86_64::instructions::interrupts::enable();
    } else {
        x86_64::instructions::interrupts::disable();
    }
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) fn host_tsc_frequency_mhz() -> Option<u32> {
    axvisor_api::arch::host_tsc_frequency_mhz()
}

#[cfg(test)]
mod tests {
    use axaddrspace::GuestPhysAddr;

    use super::*;

    #[test]
    fn real_mode_entry_keeps_normal_entry_flat() {
        assert_eq!(
            x86_real_mode_entry_state(GuestPhysAddr::from(0x8000)),
            X86RealModeEntryState {
                cs_selector: 0,
                cs_base: 0,
                rip: 0x8000,
            }
        );
    }

    #[test]
    fn real_mode_entry_maps_reset_vector_to_reset_cs_state() {
        assert_eq!(
            x86_real_mode_entry_state(GuestPhysAddr::from(0xffff_fff0)),
            X86RealModeEntryState {
                cs_selector: 0xf000,
                cs_base: 0xffff_0000,
                rip: 0xfff0,
            }
        );
    }
}
