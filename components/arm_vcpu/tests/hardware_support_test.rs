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

//! Tests for hardware support detection
//!
//! Note: These tests require aarch64 target.

#![cfg(target_arch = "aarch64")]

use arm_vcpu::has_hardware_support;

/// Test that has_hardware_support returns true
/// Currently this function always returns true as a placeholder
#[test]
fn test_has_hardware_support() {
    // Currently returns true by default
    assert!(has_hardware_support());
}
