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
#![cfg(target_arch = "aarch64")]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate log;

mod context_frame;
#[macro_use]
mod exception_utils;
mod exception;
pub mod host;
mod pcpu;
mod smc;
mod types;
mod vcpu;

pub use self::{
    host::ArmHostOps,
    pcpu::ArmPerCpu,
    types::{
        ArmAccessWidth, ArmGuestPhysAddr, ArmNestedPagingConfig, ArmSysRegAddr, ArmVcpuError,
        ArmVcpuResult, ArmVmExit,
    },
    vcpu::{
        ARM_VCPU_HOST_SP_EL0_OFFSET, ARM_VCPU_HOST_STACK_TOP_OFFSET, ARM_VCPU_TRAP_FRAME_SIZE,
        ArmVcpu, ArmVcpuCreateConfig, ArmVcpuSetupConfig,
    },
};

/// context frame for aarch64
pub type TrapFrame = context_frame::Aarch64ContextFrame;
/// Compatibility alias for existing AArch64 users.
pub type Aarch64VCpu<H> = ArmVcpu<H>;
/// Compatibility alias for existing AArch64 users.
pub type Aarch64PerCpu = ArmPerCpu;
/// Compatibility alias for existing AArch64 users.
pub type Aarch64VCpuCreateConfig = ArmVcpuCreateConfig;
/// Compatibility alias for existing AArch64 users.
pub type Aarch64VCpuSetupConfig = ArmVcpuSetupConfig;

/// Returns the maximum guest page table levels supported by the hardware.
///
/// This is determined by the physical address size:
/// - 44+ bit PA → 4 levels (48-bit IPA)
/// - < 44 bit PA → 3 levels (39-bit IPA)
pub fn max_guest_page_table_levels() -> usize {
    vcpu::max_gpt_level(vcpu::pa_bits())
}

/// Returns the physical address width reported by the current CPU.
pub fn pa_bits() -> usize {
    vcpu::pa_bits()
}

/// Return if current platform support virtualization extension.
pub fn has_hardware_support() -> bool {
    // Hint:
    // In Cortex-A78, we can use
    // [ID_AA64MMFR1_EL1](https://developer.arm.com/documentation/101430/0102/Register-descriptions/AArch64-system-registers/ID-AA64MMFR1-EL1--AArch64-Memory-Model-Feature-Register-1--EL1)
    // to get whether Virtualization Host Extensions is supported.

    // Current just return true by default.
    true
}
