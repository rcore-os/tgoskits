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
    /// Intercept COM1 PIO ports and route them to an emulated serial device.
    pub emulate_com1: bool,
}

pub mod host;
pub(crate) mod msr;
#[cfg(feature = "vmx")]
#[macro_use]
pub(crate) mod regs;
mod ept;
#[cfg(not(feature = "vmx"))]
pub(crate) mod regs;
#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) mod xstate;

cfg_if::cfg_if! {
    if #[cfg(feature = "vmx")] {
        mod vmx;
        use vmx as vendor;
        pub use vmx::{VmxExitInfo, VmxExitReason, VmxInterruptInfo, VmxIoExitInfo};

        pub use vendor::{
            VmxArchPerCpuState, VmxArchPerCpuState as X86ArchPerCpuState, VmxArchVCpu,
            VmxArchVCpu as X86ArchVCpu, X86_APIC_ACCESS_GPA, x86_apic_access_page_addr,
        };
    } else if #[cfg(feature = "svm")] {
        mod svm;
        use svm as vendor;

        pub use svm::{SvmExitCode, SvmExitInfo, SvmIntercept};
        pub use vendor::{
            SvmArchPerCpuState, SvmArchPerCpuState as X86ArchPerCpuState, SvmArchVCpu,
            SvmArchVCpu as X86ArchVCpu,
        };
    } else {
        // Fallback stub types for builds without any hypervisor backend
        // (e.g. host-fs-only). Stubs implement the required traits so that
        // downstream crates can still compile; they are never instantiated.
        mod no_backend;
        pub use no_backend::{X86ArchPerCpuState, X86ArchVCpu};
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
    u32::try_from(host::nanos_to_ticks(1_000))
        .ok()
        .filter(|&freq| freq > 0)
}
