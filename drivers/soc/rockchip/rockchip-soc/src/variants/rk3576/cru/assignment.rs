//! RK3576 host-owned UART0 clock register protection.

use alloc::{vec, vec::Vec};

use super::{Cru, clkgate_con, clksel_con};
use crate::{ClkId, ClockAssignmentProtection, ClockMmioWriteProtection};

const PCLK_UART0: ClkId = ClkId::new(135);
const SCLK_UART0: ClkId = ClkId::new(146);

impl ClockAssignmentProtection for Cru {
    fn assignment_mmio_write_protection(&self, id: ClkId) -> Option<Vec<ClockMmioWriteProtection>> {
        if !matches!(id, PCLK_UART0 | SCLK_UART0) {
            return None;
        }
        let parent = (self.read(clksel_con(60)) >> 8) & 0x7;
        Some(uart0_protection(parent))
    }
}

fn uart0_protection(parent: u32) -> Vec<ClockMmioWriteProtection> {
    let mut protections = vec![
        ClockMmioWriteProtection::MaskedWrite32 {
            offset: clkgate_con(13) as usize,
            value_mask: 1 << 10,
            write_enable_mask: 1 << 26,
        },
        ClockMmioWriteProtection::MaskedWrite32 {
            offset: clkgate_con(14) as usize,
            value_mask: 1 << 5,
            write_enable_mask: 1 << 21,
        },
        ClockMmioWriteProtection::MaskedWrite32 {
            offset: clksel_con(60) as usize,
            value_mask: 0x7ff,
            write_enable_mask: 0x7ff << 16,
        },
    ];
    if let Some(fractional_parent) = parent.checked_sub(4).filter(|parent| *parent < 3) {
        let gate_mask = 1 << (5 + fractional_parent);
        let divider_index = 21 + fractional_parent * 2;
        protections.extend_from_slice(&[
            ClockMmioWriteProtection::MaskedWrite32 {
                offset: clkgate_con(2) as usize,
                value_mask: gate_mask,
                write_enable_mask: gate_mask << 16,
            },
            ClockMmioWriteProtection::Deny {
                offset: clksel_con(divider_index) as usize,
                length: 4,
            },
            ClockMmioWriteProtection::MaskedWrite32 {
                offset: clksel_con(divider_index + 1) as usize,
                value_mask: 0x3,
                write_enable_mask: 0x3 << 16,
            },
        ]);
    }
    protections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SocType;

    #[test]
    fn uart0_fractional_parent_protects_the_complete_dependency() {
        let mut registers = vec![0_u32; 0x5_0000 / 4];
        registers[clksel_con(60) as usize / 4] = 4 << 8;
        let base = core::ptr::NonNull::new(registers.as_mut_ptr().cast()).unwrap();
        let cru = crate::Cru::new(SocType::Rk3576, base, base);
        let expected = vec![
            ClockMmioWriteProtection::MaskedWrite32 {
                offset: 0x834,
                value_mask: 0x400,
                write_enable_mask: 0x0400_0000,
            },
            ClockMmioWriteProtection::MaskedWrite32 {
                offset: 0x838,
                value_mask: 0x20,
                write_enable_mask: 0x0020_0000,
            },
            ClockMmioWriteProtection::MaskedWrite32 {
                offset: 0x3f0,
                value_mask: 0x7ff,
                write_enable_mask: 0x07ff_0000,
            },
            ClockMmioWriteProtection::MaskedWrite32 {
                offset: 0x808,
                value_mask: 0x20,
                write_enable_mask: 0x0020_0000,
            },
            ClockMmioWriteProtection::Deny {
                offset: 0x354,
                length: 4,
            },
            ClockMmioWriteProtection::MaskedWrite32 {
                offset: 0x358,
                value_mask: 0x3,
                write_enable_mask: 0x0003_0000,
            },
        ];

        assert_eq!(
            cru.assignment_mmio_write_protection(PCLK_UART0),
            Some(expected.clone())
        );
        assert_eq!(
            cru.assignment_mmio_write_protection(SCLK_UART0),
            Some(expected)
        );
        assert_eq!(
            cru.assignment_mmio_write_protection(super::super::CCLK_SRC_SDMMC0),
            None
        );
    }
}
