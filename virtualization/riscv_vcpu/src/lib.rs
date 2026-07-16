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
#![cfg(target_arch = "riscv64")]
#![feature(riscv_ext_intrinsics)]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate log;
extern crate alloc;

mod consts;
/// The Control and Status Registers (CSRs) for a RISC-V hypervisor.
mod detect;
mod guest_mem;
pub mod host;
mod percpu;
mod registers;
mod regs;
mod sbi_console;
mod trap;
pub mod types;
mod vcpu;
mod vpmu;

pub use detect::{detect_h_extension as has_hardware_support, max_guest_page_table_levels};
pub use regs::GprIndex;
pub use types::{
    RiscvAccessFlags, RiscvAccessWidth, RiscvGuestPhysAddr, RiscvGuestVirtAddr, RiscvHostPhysAddr,
    RiscvHostVirtAddr, RiscvNestedPagingConfig, RiscvVcpuError, RiscvVcpuId, RiscvVcpuResult,
    RiscvVmExit, RiscvVmId,
};

pub use self::{
    host::RiscvHostOps,
    percpu::{RISCVPerCpu, RiscvPerCpu},
    vcpu::{RISCVVCpu, RiscvBoundExit, RiscvVCpu, RiscvVcpu},
};

/// Extension ID for hypercall, defined by ourselves.
/// `0x48`, `0x56`, `0x43` is "HVC" in ASCII.
///
/// Borrowed from the design of `eid_from_str` in [sbi-spec](https://github.com/rustsbi/rustsbi/blob/62ab2e498ca66cdf75ce049c9dbc2f1862874553/sbi-spec/src/lib.rs#L51)
pub const EID_HVC: usize = 0x485643;

/// Configuration for creating a new `RiscvVcpu`.
#[derive(Clone, Debug)]
pub struct RiscvVcpuCreateConfig {
    /// The ID of the vCPU, default to `0`.
    pub hart_id: usize,
    /// The physical address of the device tree blob.
    /// Default to `0x9000_0000`.
    pub dtb_addr: usize,
}

impl Default for RiscvVcpuCreateConfig {
    fn default() -> Self {
        Self {
            hart_id: 0,
            dtb_addr: 0x9000_0000,
        }
    }
}

/// Backward-compatible creation config alias.
pub type RISCVVCpuCreateConfig = RiscvVcpuCreateConfig;
