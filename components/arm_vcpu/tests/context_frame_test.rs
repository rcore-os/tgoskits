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

//! Tests for context_frame module
//!
//! Note: These tests require aarch64 target and may need to run on actual hardware or QEMU.

#![cfg(target_arch = "aarch64")]

use arm_vcpu::TrapFrame;

/// Test that the default context frame has all GPRs set to zero
#[test]
fn test_context_frame_default() {
    let ctx = TrapFrame::default();

    // All GPRs should be zero
    for i in 0..31 {
        assert_eq!(ctx.gpr[i], 0, "GPR {} should be 0", i);
    }

    // sp_el0 should be zero
    assert_eq!(ctx.sp_el0, 0);

    // elr should be zero
    assert_eq!(ctx.elr, 0);

    // spsr should have the expected default value (EL1h mode with interrupts masked)
    assert_ne!(ctx.spsr, 0);
}

/// Test setting GPR values directly
#[test]
fn test_context_frame_gpr_access() {
    let mut ctx = TrapFrame::default();

    // Test setting GPRs 0-30 directly
    for i in 0..=30 {
        ctx.gpr[i] = 0xDEADBEEF;
        assert_eq!(ctx.gpr[i], 0xDEADBEEF, "GPR {} should be 0xDEADBEEF", i);
    }
}

/// Test setting the argument (x0 register) directly
#[test]
fn test_context_frame_set_argument_direct() {
    let mut ctx = TrapFrame::default();

    ctx.gpr[0] = 0x12345678;
    assert_eq!(ctx.gpr[0], 0x12345678);
}

/// Test setting and getting the exception program counter directly
#[test]
fn test_context_frame_elr_access() {
    let mut ctx = TrapFrame::default();

    ctx.elr = 0x8000;
    assert_eq!(ctx.elr, 0x8000);
}

/// Test that the context frame is properly aligned and sized
#[test]
fn test_context_frame_layout() {
    use core::mem::{align_of, size_of};

    // Check size: 31 GPRs + sp_el0 + elr + spsr = 34 u64s = 272 bytes
    assert_eq!(size_of::<TrapFrame>(), 34 * 8);

    // The struct should be aligned to 8 bytes (u64 alignment)
    assert!(align_of::<TrapFrame>() >= 8);
}

/// Test context frame clone and copy
#[test]
fn test_context_frame_clone() {
    let mut ctx1 = TrapFrame::default();
    ctx1.gpr[0] = 0x1234;
    ctx1.gpr[1] = 0x5678;
    ctx1.elr = 0x8000;

    let ctx2 = ctx1;

    assert_eq!(ctx2.gpr[0], 0x1234);
    assert_eq!(ctx2.gpr[1], 0x5678);
    assert_eq!(ctx2.elr, 0x8000);
}
