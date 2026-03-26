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

//! Tests for vcpu configuration structures
//!
//! Note: These tests require aarch64 target.

#![cfg(target_arch = "aarch64")]

use arm_vcpu::{Aarch64VCpuCreateConfig, Aarch64VCpuSetupConfig};

/// Test default Aarch64VCpuCreateConfig
#[test]
fn test_vcpu_create_config_default() {
    let config = Aarch64VCpuCreateConfig::default();

    assert_eq!(config.mpidr_el1, 0);
    assert_eq!(config.dtb_addr, 0);
}

/// Test setting Aarch64VCpuCreateConfig fields
#[test]
fn test_vcpu_create_config_fields() {
    let config = Aarch64VCpuCreateConfig {
        mpidr_el1: 0x80000000,
        dtb_addr: 0x40000000,
    };

    assert_eq!(config.mpidr_el1, 0x80000000);
    assert_eq!(config.dtb_addr, 0x40000000);
}

/// Test default Aarch64VCpuSetupConfig
#[test]
fn test_vcpu_setup_config_default() {
    let config = Aarch64VCpuSetupConfig::default();

    assert_eq!(config.passthrough_interrupt, false);
    assert_eq!(config.passthrough_timer, false);
}

/// Test setting Aarch64VCpuSetupConfig fields
#[test]
fn test_vcpu_setup_config_fields() {
    let config = Aarch64VCpuSetupConfig {
        passthrough_interrupt: true,
        passthrough_timer: true,
    };

    assert_eq!(config.passthrough_interrupt, true);
    assert_eq!(config.passthrough_timer, true);
}
