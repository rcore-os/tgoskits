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
#![feature(doc_cfg)]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate log;

mod context_frame;
#[macro_use]
mod exception_utils;
mod exception;
mod pcpu;
mod smc;
mod vcpu;

pub use self::pcpu::Aarch64PerCpu;
pub use self::vcpu::{Aarch64VCpu, Aarch64VCpuCreateConfig, Aarch64VCpuSetupConfig};

/// context frame for aarch64
pub type TrapFrame = context_frame::Aarch64ContextFrame;

/// Return if current platform support virtualization extension.
pub fn has_hardware_support() -> bool {
    // Hint:
    // In Cortex-A78, we can use
    // [ID_AA64MMFR1_EL1](https://developer.arm.com/documentation/101430/0102/Register-descriptions/AArch64-system-registers/ID-AA64MMFR1-EL1--AArch64-Memory-Model-Feature-Register-1--EL1)
    // to get whether Virtualization Host Extensions is supported.

    // Current just return true by default.
    true
}
