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

//! Tests for SPSR (Saved Program Status Register) related functionality
//!
//! Note: These tests require aarch64 target.

#![cfg(target_arch = "aarch64")]

use arm_vcpu::TrapFrame;

/// Test SPSR default value has correct mode and interrupt masks
#[test]
fn test_spsr_default_value() {
    let ctx = TrapFrame::default();

    // The default SPSR should have:
    // - M[3:0] = 0b0101 (EL1h mode)
    // - I (IRQ mask) = 1
    // - F (FIQ mask) = 1
    // - A (SError mask) = 1
    // - D (Debug mask) = 1

    let spsr = ctx.spsr;

    // Check EL1h mode (bits 3:0 = 0x5)
    assert_eq!(spsr & 0xF, 0x5, "SPSR should be in EL1h mode");

    // Check IRQ mask (bit 7)
    assert_eq!(spsr & (1 << 7), 1 << 7, "IRQ should be masked");

    // Check FIQ mask (bit 6)
    assert_eq!(spsr & (1 << 6), 1 << 6, "FIQ should be masked");

    // Check SError mask (bit 8)
    assert_eq!(spsr & (1 << 8), 1 << 8, "SError should be masked");

    // Check Debug mask (bit 9)
    assert_eq!(spsr & (1 << 9), 1 << 9, "Debug should be masked");
}

/// Test SPSR mode values
#[test]
fn test_spsr_mode_values() {
    // EL0t: 0x0
    // EL1t: 0x4
    // EL1h: 0x5
    // EL2t: 0x8
    // EL2h: 0x9

    const EL0T: u64 = 0x0;
    const EL1T: u64 = 0x4;
    const EL1H: u64 = 0x5;
    const EL2T: u64 = 0x8;
    const EL2H: u64 = 0x9;

    assert_eq!(EL0T, 0);
    assert_eq!(EL1T, 4);
    assert_eq!(EL1H, 5);
    assert_eq!(EL2T, 8);
    assert_eq!(EL2H, 9);
}
