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

//! Unit tests for individual RISC-V hypervisor registers

use riscv_h::register::{hcounteren, hie, hip, hvip, vsatp, vsie, vsip, vstvec};

// ============================================================================
// Hypervisor Control Registers Tests
// ============================================================================

mod hvip_tests {
    use super::*;

    #[test]
    fn test_hvip_bit_fields() {
        let mut hvip = hvip::Hvip::from_bits(0);

        // Test VSSIP (bit 2)
        assert!(!hvip.vssip());
        hvip.set_vssip(true);
        assert!(hvip.vssip());
        assert_eq!(hvip.bits(), 0b100);

        // Test VSTIP (bit 6)
        hvip.set_vstip(true);
        assert!(hvip.vstip());
        assert_eq!(hvip.bits(), 0b100_0100);

        // Test VSEIP (bit 10)
        hvip.set_vseip(true);
        assert!(hvip.vseip());
        // bit 2 + bit 6 + bit 10 = 4 + 64 + 1024 = 1092
        assert_eq!(hvip.bits(), (1 << 2) | (1 << 6) | (1 << 10));
    }

    #[test]
    fn test_hvip_bit_isolation() {
        let mut hvip = hvip::Hvip::from_bits(0);

        // Set all interrupt pending bits
        hvip.set_vssip(true);
        hvip.set_vstip(true);
        hvip.set_vseip(true);

        // Clear one at a time and verify others unchanged
        hvip.set_vssip(false);
        assert!(!hvip.vssip());
        assert!(hvip.vstip());
        assert!(hvip.vseip());

        hvip.set_vstip(false);
        assert!(!hvip.vssip());
        assert!(!hvip.vstip());
        assert!(hvip.vseip());
    }
}

mod hcounteren_tests {
    use super::*;

    #[test]
    fn test_hcounteren_basic_fields() {
        let mut hcounteren = hcounteren::Hcounteren::from_bits(0);

        // Test CY (bit 0)
        hcounteren.set_cy(true);
        assert!(hcounteren.cy());

        // Test TM (bit 1)
        hcounteren.set_tm(true);
        assert!(hcounteren.tm());

        // Test IR (bit 2)
        hcounteren.set_ir(true);
        assert!(hcounteren.ir());

        assert_eq!(hcounteren.bits(), 0b111);
    }

    #[test]
    fn test_hcounteren_hpm_fields() {
        let mut hcounteren = hcounteren::Hcounteren::from_bits(0);

        // Test a few HPM counters
        hcounteren.set_hpm3(true);
        hcounteren.set_hpm10(true);
        hcounteren.set_hpm31(true);

        assert!(hcounteren.hpm3());
        assert!(hcounteren.hpm10());
        assert!(hcounteren.hpm31());

        // Verify bits are at correct positions
        assert_eq!(hcounteren.bits() & (1 << 3), 1 << 3); // hpm3
        assert_eq!(hcounteren.bits() & (1 << 10), 1 << 10); // hpm10
        assert_eq!(hcounteren.bits() & (1 << 31), 1 << 31); // hpm31
    }
}

mod hie_tests {
    use super::*;

    #[test]
    fn test_hie_bit_fields() {
        let mut hie = hie::Hie::from_bits(0);

        // Test VSSIE (bit 2)
        hie.set_vssie(true);
        assert!(hie.vssie());

        // Test VSTIE (bit 6)
        hie.set_vstie(true);
        assert!(hie.vstie());

        // Test VSEIE (bit 10)
        hie.set_vseie(true);
        assert!(hie.vseie());

        // Test SGEIE (bit 12)
        hie.set_sgeie(true);
        assert!(hie.sgeie());

        assert_eq!(hie.bits(), (1 << 2) | (1 << 6) | (1 << 10) | (1 << 12));
    }

    #[test]
    fn test_hie_bit_isolation() {
        let mut hie = hie::Hie::from_bits(0);

        hie.set_vssie(true);
        hie.set_vstie(true);
        hie.set_vseie(true);
        hie.set_sgeie(true);

        // Clear one at a time
        hie.set_vssie(false);
        assert!(!hie.vssie());
        assert!(hie.vstie());
        assert!(hie.vseie());
        assert!(hie.sgeie());
    }
}

mod hip_tests {
    use super::*;

    #[test]
    fn test_hip_bit_fields() {
        let mut hip = hip::Hip::from_bits(0);

        // Test VSSIP (bit 2)
        hip.set_vssip(true);
        assert!(hip.vssip());

        // Test VSTIP (bit 6)
        hip.set_vstip(true);
        assert!(hip.vstip());

        // Test VSEIP (bit 10)
        hip.set_vseip(true);
        assert!(hip.vseip());

        // Test SGEIP (bit 12)
        hip.set_sgeip(true);
        assert!(hip.sgeip());

        assert_eq!(hip.bits(), (1 << 2) | (1 << 6) | (1 << 10) | (1 << 12));
    }

    #[test]
    fn test_hip_bit_isolation() {
        let mut hip = hip::Hip::from_bits(0);

        hip.set_vssip(true);
        hip.set_vstip(true);
        hip.set_vseip(true);
        hip.set_sgeip(true);

        // Clear one at a time
        hip.set_sgeip(false);
        assert!(hip.vssip());
        assert!(hip.vstip());
        assert!(hip.vseip());
        assert!(!hip.sgeip());
    }
}

// ============================================================================
// Virtual Supervisor Registers Tests
// ============================================================================

mod vsatp_tests {
    use super::*;

    #[test]
    fn test_vsatp_mode() {
        let mut vsatp = vsatp::Vsatp::from_bits(0);

        vsatp.set_mode(vsatp::HgatpValues::Bare);
        assert!(matches!(vsatp.mode(), vsatp::HgatpValues::Bare));

        vsatp.set_mode(vsatp::HgatpValues::Sv39x4);
        assert!(matches!(vsatp.mode(), vsatp::HgatpValues::Sv39x4));

        vsatp.set_mode(vsatp::HgatpValues::Sv48x4);
        assert!(matches!(vsatp.mode(), vsatp::HgatpValues::Sv48x4));
    }

    #[test]
    fn test_vsatp_asid() {
        let mut vsatp = vsatp::Vsatp::from_bits(0);

        // ASID is 16 bits (bits 44-59)
        vsatp.set_asid(0);
        assert_eq!(vsatp.asid(), 0);

        vsatp.set_asid(0xFFFF);
        assert_eq!(vsatp.asid(), 0xFFFF);

        vsatp.set_asid(0x1234);
        assert_eq!(vsatp.asid(), 0x1234);
    }

    #[test]
    fn test_vsatp_ppn() {
        let mut vsatp = vsatp::Vsatp::from_bits(0);

        // PPN is 44 bits (bits 0-43)
        vsatp.set_ppn(0);
        assert_eq!(vsatp.ppn(), 0);

        vsatp.set_ppn(0xFFFFFFFFFFF);
        assert_eq!(vsatp.ppn(), 0xFFFFFFFFFFF);
    }

    #[test]
    fn test_vsatp_field_isolation() {
        let mut vsatp = vsatp::Vsatp::from_bits(0);

        vsatp.set_mode(vsatp::HgatpValues::Sv48x4);
        vsatp.set_asid(0xABCD);
        vsatp.set_ppn(0x123456789);

        // Verify all fields are independent
        assert!(matches!(vsatp.mode(), vsatp::HgatpValues::Sv48x4));
        assert_eq!(vsatp.asid(), 0xABCD);
        assert_eq!(vsatp.ppn(), 0x123456789);

        // Modify one field shouldn't affect others
        vsatp.set_asid(0);
        assert!(matches!(vsatp.mode(), vsatp::HgatpValues::Sv48x4));
        assert_eq!(vsatp.asid(), 0);
        assert_eq!(vsatp.ppn(), 0x123456789);
    }
}

mod vstvec_tests {
    use super::*;

    #[test]
    fn test_vstvec_base() {
        let mut vstvec = vstvec::Vstvec::from_bits(0);

        // Base is bits 2-63, must be 4-byte aligned
        vstvec.set_base(0x1000);
        assert_eq!(vstvec.base(), 0x1000);

        vstvec.set_base(0x80000000);
        assert_eq!(vstvec.base(), 0x80000000);
    }

    #[test]
    fn test_vstvec_mode() {
        let mut vstvec = vstvec::Vstvec::from_bits(0);

        // Mode is bits 0-1
        vstvec.set_mode(0); // Direct
        assert_eq!(vstvec.mode(), 0);

        vstvec.set_mode(1); // Vectored
        assert_eq!(vstvec.mode(), 1);
    }

    #[test]
    fn test_vstvec_field_isolation() {
        let mut vstvec = vstvec::Vstvec::from_bits(0);

        vstvec.set_base(0x12345678);
        vstvec.set_mode(1);

        // base() returns the raw bits 2-63 value
        assert_eq!(vstvec.base(), 0x12345678);
        assert_eq!(vstvec.mode(), 1);
    }
}

mod vsie_tests {
    use super::*;

    #[test]
    fn test_vsie_bit_fields() {
        let mut vsie = vsie::Vsie::from_bits(0);

        // Test SSIE (bit 1)
        vsie.set_ssie(true);
        assert!(vsie.ssie());

        // Test STIE (bit 5)
        vsie.set_stie(true);
        assert!(vsie.stie());

        // Test SEIE (bit 9)
        vsie.set_seie(true);
        assert!(vsie.seie());

        assert_eq!(vsie.bits(), (1 << 1) | (1 << 5) | (1 << 9));
    }

    #[test]
    fn test_vsie_bit_isolation() {
        let mut vsie = vsie::Vsie::from_bits(0);

        vsie.set_ssie(true);
        vsie.set_stie(true);
        vsie.set_seie(true);

        vsie.set_ssie(false);
        assert!(!vsie.ssie());
        assert!(vsie.stie());
        assert!(vsie.seie());
    }
}

mod vsip_tests {
    use super::*;

    #[test]
    fn test_vsip_bit_fields() {
        let mut vsip = vsip::Vsip::from_bits(0);

        // Test SSIP (bit 1)
        vsip.set_ssip(true);
        assert!(vsip.ssip());

        // Test STIP (bit 5)
        vsip.set_stip(true);
        assert!(vsip.stip());

        // Test SEIP (bit 9)
        vsip.set_seip(true);
        assert!(vsip.seip());

        assert_eq!(vsip.bits(), (1 << 1) | (1 << 5) | (1 << 9));
    }

    #[test]
    fn test_vsip_bit_isolation() {
        let mut vsip = vsip::Vsip::from_bits(0);

        vsip.set_ssip(true);
        vsip.set_stip(true);
        vsip.set_seip(true);

        vsip.set_ssip(false);
        assert!(!vsip.ssip());
        assert!(vsip.stip());
        assert!(vsip.seip());
    }
}

// ============================================================================
// Copy/Clone Trait Tests
// ============================================================================

mod trait_tests {
    use super::*;

    #[test]
    fn test_all_registers_copy_clone() {
        // Test that all register types implement Copy and Clone
        let hvip = hvip::Hvip::from_bits(0x123);
        let _copy = hvip;
        let _clone = hvip.clone();

        let hcounteren = hcounteren::Hcounteren::from_bits(0xFF);
        let _copy = hcounteren;
        let _clone = hcounteren.clone();

        let vsatp = vsatp::Vsatp::from_bits(0xABC);
        let _copy = vsatp;
        let _clone = vsatp.clone();

        let vstvec = vstvec::Vstvec::from_bits(0x1000);
        let _copy = vstvec;
        let _clone = vstvec.clone();
    }

    #[test]
    fn test_all_registers_debug() {
        // Test that all register types implement Debug
        let hvip = hvip::Hvip::from_bits(0x123);
        assert!(format!("{:?}", hvip).contains("Hvip"));

        let hcounteren = hcounteren::Hcounteren::from_bits(0xFF);
        assert!(format!("{:?}", hcounteren).contains("Hcounteren"));

        let vsatp = vsatp::Vsatp::from_bits(0xABC);
        assert!(format!("{:?}", vsatp).contains("Vsatp"));

        let vstvec = vstvec::Vstvec::from_bits(0x1000);
        assert!(format!("{:?}", vstvec).contains("Vstvec"));
    }
}
