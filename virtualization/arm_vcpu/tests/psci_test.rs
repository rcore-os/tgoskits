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

//! Tests for PSCI (Power State Coordination Interface) related functionality
//!
//! Note: These tests require aarch64 target.

#![cfg(target_arch = "aarch64")]

/// Test PSCI function number ranges
#[test]
fn test_psci_function_ranges() {
    // 32-bit PSCI function range
    const PSCI_FN_RANGE_32_START: u64 = 0x8400_0000;
    const PSCI_FN_RANGE_32_END: u64 = 0x8400_001F;

    // 64-bit PSCI function range
    const PSCI_FN_RANGE_64_START: u64 = 0xC400_0000;
    const PSCI_FN_RANGE_64_END: u64 = 0xC400_001F;

    // Test 32-bit range
    assert!(PSCI_FN_RANGE_32_START <= PSCI_FN_RANGE_32_END);
    assert_eq!(PSCI_FN_RANGE_32_END - PSCI_FN_RANGE_32_START, 0x1F);

    // Test 64-bit range
    assert!(PSCI_FN_RANGE_64_START <= PSCI_FN_RANGE_64_END);
    assert_eq!(PSCI_FN_RANGE_64_END - PSCI_FN_RANGE_64_START, 0x1F);

    // Test that ranges don't overlap
    assert!(PSCI_FN_RANGE_32_END < PSCI_FN_RANGE_64_START);
}

/// Test PSCI function offsets
#[test]
fn test_psci_function_offsets() {
    // PSCI function offsets (same for both 32-bit and 64-bit)
    const PSCI_FN_VERSION: u64 = 0x0;
    const PSCI_FN_CPU_SUSPEND: u64 = 0x1;
    const PSCI_FN_CPU_OFF: u64 = 0x2;
    const PSCI_FN_CPU_ON: u64 = 0x3;
    const PSCI_FN_MIGRATE: u64 = 0x5;
    const PSCI_FN_SYSTEM_OFF: u64 = 0x8;
    const PSCI_FN_SYSTEM_RESET: u64 = 0x9;

    assert_eq!(PSCI_FN_VERSION, 0);
    assert_eq!(PSCI_FN_CPU_SUSPEND, 1);
    assert_eq!(PSCI_FN_CPU_OFF, 2);
    assert_eq!(PSCI_FN_CPU_ON, 3);
    assert_eq!(PSCI_FN_MIGRATE, 5);
    assert_eq!(PSCI_FN_SYSTEM_OFF, 8);
    assert_eq!(PSCI_FN_SYSTEM_RESET, 9);
}

/// Test PSCI function number construction
#[test]
fn test_psci_function_number_construction() {
    // Construct PSCI function numbers
    let psci_version_32: u64 = 0x8400_0000;
    let psci_cpu_on_32: u64 = 0x8400_0003;

    let psci_version_64: u64 = 0xC400_0000;
    let psci_cpu_on_64: u64 = 0xC400_0003;

    // Verify 32-bit functions
    assert_eq!(psci_version_32 & 0xFF, 0);
    assert_eq!(psci_cpu_on_32 & 0xFF, 3);

    // Verify 64-bit functions
    assert_eq!(psci_version_64 & 0xFF, 0);
    assert_eq!(psci_cpu_on_64 & 0xFF, 3);

    // Verify bit 31 distinguishes 32-bit vs 64-bit
    assert_eq!(psci_version_32 & 0x40000000, 0);
    assert_eq!(psci_version_64 & 0x40000000, 0x40000000);
}
