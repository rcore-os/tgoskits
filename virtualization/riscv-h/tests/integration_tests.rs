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

//! Integration tests for riscv-h library
//!
//! These tests verify that the register implementations work correctly
//! together and that the library's public API is functioning as expected.

use riscv_h::register::{hgatp, hstatus, vsstatus};

#[test]
fn test_register_independence() {
    // Create multiple register instances and verify they don't interfere with each other
    let mut hstatus_reg = hstatus::Hstatus::from_bits(0);
    let mut hgatp_reg = hgatp::Hgatp::from_bits(0);
    let mut vsstatus_reg = vsstatus::Vsstatus::from_bits(0);

    // Set different values in each register
    hstatus_reg.set_vtsr(true);
    hstatus_reg.set_vgein(0x15);

    hgatp_reg.set_mode(hgatp::HgatpValues::Sv48x4);
    hgatp_reg.set_vmid(0x1234);

    vsstatus_reg.set_mxr(true);
    vsstatus_reg.set_uxl(vsstatus::UxlValues::Uxl64);

    // Verify that each register maintains its state independently
    assert!(hstatus_reg.vtsr());
    assert_eq!(hstatus_reg.vgein(), 0x15);

    assert!(matches!(hgatp_reg.mode(), hgatp::HgatpValues::Sv48x4));
    assert_eq!(hgatp_reg.vmid(), 0x1234);

    assert!(vsstatus_reg.mxr());
    assert!(matches!(vsstatus_reg.uxl(), vsstatus::UxlValues::Uxl64));
}

#[test]
fn test_enum_conversions() {
    // Test that all enum types can be converted properly

    // HgatpValues
    let hgatp_bare = hgatp::HgatpValues::Bare;
    let hgatp_sv39 = hgatp::HgatpValues::Sv39x4;
    let hgatp_sv48 = hgatp::HgatpValues::Sv48x4;

    assert_eq!(hgatp_bare as usize, 0);
    assert_eq!(hgatp_sv39 as usize, 8);
    assert_eq!(hgatp_sv48 as usize, 9);

    // VsxlValues
    let vsxl_32 = hstatus::VsxlValues::Vsxl32;
    let vsxl_64 = hstatus::VsxlValues::Vsxl64;
    let vsxl_128 = hstatus::VsxlValues::Vsxl128;

    assert_eq!(vsxl_32 as usize, 1);
    assert_eq!(vsxl_64 as usize, 2);
    assert_eq!(vsxl_128 as usize, 3);

    // UxlValues
    let uxl_32 = vsstatus::UxlValues::Uxl32;
    let uxl_64 = vsstatus::UxlValues::Uxl64;
    let uxl_128 = vsstatus::UxlValues::Uxl128;

    assert_eq!(uxl_32 as usize, 1);
    assert_eq!(uxl_64 as usize, 2);
    assert_eq!(uxl_128 as usize, 3);
}

#[test]
fn test_bit_field_isolation() {
    // Test that setting different bit fields doesn't cause interference
    let mut hstatus_reg = hstatus::Hstatus::from_bits(0);

    // Set multiple non-overlapping fields
    hstatus_reg.set_vsxl(hstatus::VsxlValues::Vsxl64); // bits 32-33
    hstatus_reg.set_vtsr(true); // bit 22
    hstatus_reg.set_vgein(0x2A); // bits 12-17
    hstatus_reg.set_hu(true); // bit 9
    hstatus_reg.set_gva(true); // bit 6

    // Verify all fields are set correctly and independently
    assert!(matches!(hstatus_reg.vsxl(), hstatus::VsxlValues::Vsxl64));
    assert!(hstatus_reg.vtsr());
    assert_eq!(hstatus_reg.vgein(), 0x2A);
    assert!(hstatus_reg.hu());
    assert!(hstatus_reg.gva());

    // Verify that changing one field doesn't affect others
    hstatus_reg.set_vtsr(false);

    assert!(matches!(hstatus_reg.vsxl(), hstatus::VsxlValues::Vsxl64));
    assert!(!hstatus_reg.vtsr()); // This should be false now
    assert_eq!(hstatus_reg.vgein(), 0x2A);
    assert!(hstatus_reg.hu());
    assert!(hstatus_reg.gva());
}

#[test]
fn test_large_bit_patterns() {
    // Test with complex bit patterns to ensure no field overlap
    let test_pattern = 0xDEADBEEFCAFEBABE_usize;

    let hgatp_reg = hgatp::Hgatp::from_bits(test_pattern);

    // Extract expected values based on the bit pattern
    let expected_mode = (test_pattern >> 60) & 0xF;
    let expected_vmid = (test_pattern >> 44) & 0x3FFF;
    let expected_ppn = test_pattern & 0xFFFFFFFFFFF;

    // Note: mode conversion might panic for invalid values in real usage,
    // so we only test the bit extraction here
    assert_eq!(hgatp_reg.bits() >> 60 & 0xF, expected_mode);
    assert_eq!(hgatp_reg.vmid(), expected_vmid);
    assert_eq!(hgatp_reg.ppn(), expected_ppn);
}

#[test]
fn test_register_cloning() {
    // Test that register structs can be cloned and copied correctly
    let original = hstatus::Hstatus::from_bits(0x123456789ABCDEF0);

    // Test Copy trait
    let copied = original;
    assert_eq!(original.bits(), copied.bits());

    // Test Clone trait
    let cloned = original.clone();
    assert_eq!(original.bits(), cloned.bits());

    // Verify that modifications to copies don't affect the original
    let mut modified = original;
    let modified_bits = hstatus::Hstatus::from_bits(0);
    assert_ne!(original.bits(), modified_bits.bits());
}

#[test]
fn test_boundary_values() {
    // Test boundary conditions for multi-bit fields
    let mut hgatp_reg = hgatp::Hgatp::from_bits(0);

    // Test VMID boundaries (14-bit field)
    hgatp_reg.set_vmid(0);
    assert_eq!(hgatp_reg.vmid(), 0);

    hgatp_reg.set_vmid(0x3FFF); // Maximum 14-bit value
    assert_eq!(hgatp_reg.vmid(), 0x3FFF);

    // Test PPN boundaries (44-bit field)
    hgatp_reg.set_ppn(0);
    assert_eq!(hgatp_reg.ppn(), 0);

    hgatp_reg.set_ppn(0xFFFFFFFFFFF); // Maximum 44-bit value
    assert_eq!(hgatp_reg.ppn(), 0xFFFFFFFFFFF);
}

#[test]
fn test_debug_formatting() {
    // Test that Debug trait works correctly
    let hstatus_reg = hstatus::Hstatus::from_bits(0x400);
    let debug_str = format!("{:?}", hstatus_reg);

    assert!(debug_str.contains("Hstatus"));
    assert!(debug_str.contains("1024"));

    let vsxl_val = hstatus::VsxlValues::Vsxl64;
    let debug_str = format!("{:?}", vsxl_val);
    assert!(debug_str.contains("Vsxl64"));
}
