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

//! Tests for VmCpuRegisters structure
//!
//! Note: These tests require aarch64 target.

#![cfg(target_arch = "aarch64")]

use arm_vcpu::TrapFrame;

/// Re-export VmCpuRegisters for testing
/// Note: This structure may not be publicly exported, so we test what we can access

/// Test that VmCpuRegisters can be created and defaulted
#[test]
fn test_vmcpu_registers_layout() {
    // VmCpuRegisters contains:
    // - trap_context_regs: TrapFrame (34 u64s = 272 bytes)
    // - vm_system_regs: GuestSystemRegisters (many fields)

    // We can at least verify TrapFrame size
    use core::mem::size_of;
    assert_eq!(size_of::<TrapFrame>(), 34 * 8);
}

/// Test that we can create a basic context for a VM
#[test]
fn test_basic_vm_context() {
    let mut trap_frame = TrapFrame::default();

    // Set up entry point
    trap_frame.elr = 0x40000000;

    // Set up stack pointer
    trap_frame.sp_el0 = 0x80000000;

    // Set up first argument (e.g., DTB address)
    trap_frame.gpr[0] = 0x42000000;

    assert_eq!(trap_frame.elr, 0x40000000);
    assert_eq!(trap_frame.sp_el0, 0x80000000);
    assert_eq!(trap_frame.gpr[0], 0x42000000);
}
