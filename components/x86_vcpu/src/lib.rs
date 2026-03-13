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

extern crate alloc;

#[cfg(test)]
mod test_utils;

pub(crate) mod msr;
#[macro_use]
pub(crate) mod regs;
mod ept;

cfg_if::cfg_if! {
    if #[cfg(feature = "vmx")] {
        mod vmx;
        use vmx as vender;
        pub use vmx::{VmxExitInfo, VmxExitReason, VmxInterruptInfo, VmxIoExitInfo};

        pub use vender::VmxArchVCpu;
        pub use vender::VmxArchPerCpuState;
    }
}

pub use ept::GuestPageWalkInfo;
pub use regs::GeneralRegisters;
pub use vender::has_hardware_support;
