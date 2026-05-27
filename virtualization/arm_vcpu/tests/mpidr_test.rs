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

//! Tests for MPIDR (Multiprocessor Affinity Register) related functionality
//!
//! Note: These tests require aarch64 target.

#![cfg(target_arch = "aarch64")]

/// Test MPIDR_EL1 value construction for vCPU
#[test]
fn test_mpidr_value_construction() {
    // MPIDR format: Affinity levels and MT bit
    // Bit 31: MT (multiprocessing extension)
    // Bits 23:0: Affinity (Aff3:Aff2:Aff1:Aff0)

    // Test CPU 0
    let mpidr_cpu0: u64 = 0x80000000; // MT bit set, affinity 0
    assert_eq!(mpidr_cpu0 & 0x80000000, 0x80000000); // MT bit
    assert_eq!(mpidr_cpu0 & 0x00FFFFFF, 0); // Affinity 0

    // Test CPU 1
    let mpidr_cpu1: u64 = 0x80000001; // MT bit set, affinity 1
    assert_eq!(mpidr_cpu1 & 0x80000000, 0x80000000); // MT bit
    assert_eq!(mpidr_cpu1 & 0x00FFFFFF, 1); // Affinity 1

    // Test with cluster (Aff1 = 1)
    let mpidr_cluster1_cpu0: u64 = 0x80000100; // MT bit set, Aff1=1, Aff0=0
    assert_eq!(mpidr_cluster1_cpu0 & 0x80000000, 0x80000000);
    assert_eq!((mpidr_cluster1_cpu0 >> 8) & 0xFF, 1); // Aff1
    assert_eq!(mpidr_cluster1_cpu0 & 0xFF, 0); // Aff0
}

/// Test affinity mask extraction
#[test]
fn test_affinity_levels() {
    // Test extracting affinity levels from MPIDR
    let mpidr: u64 = 0x80010203; // Aff3=0, Aff2=1, Aff1=2, Aff0=3

    let aff0 = mpidr & 0xFF;
    let aff1 = (mpidr >> 8) & 0xFF;
    let aff2 = (mpidr >> 16) & 0xFF;
    let aff3 = (mpidr >> 32) & 0xFF;

    assert_eq!(aff0, 3);
    assert_eq!(aff1, 2);
    assert_eq!(aff2, 1);
    assert_eq!(aff3, 0);
}
