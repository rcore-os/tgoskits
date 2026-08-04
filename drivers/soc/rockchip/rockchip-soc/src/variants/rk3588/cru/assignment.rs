//! RK3588 host-owned UART clock register protection.

use alloc::vec::Vec;

use crate::{
    ClkId, ClockAssignmentProtection, ClockMmioWriteProtection,
    variants::rk3588::cru::{Cru, clkgate_con, clksel_con},
};

const PCLK_UART1_ID: u64 = 171;
const PCLK_UART9_ID: u64 = 179;
const CLK_UART1_SRC_ID: u64 = 180;
const SCLK_UART9_ID: u64 = 215;

impl ClockAssignmentProtection for Cru {
    fn assignment_mmio_write_protection(&self, id: ClkId) -> Option<Vec<ClockMmioWriteProtection>> {
        let uart = uart_number(id)?;
        Some(uart_protection(uart))
    }
}

fn uart_number(id: ClkId) -> Option<u32> {
    let raw = id.value();
    if (PCLK_UART1_ID..=PCLK_UART9_ID).contains(&raw) {
        return Some((raw - PCLK_UART1_ID + 1) as u32);
    }
    (CLK_UART1_SRC_ID..=SCLK_UART9_ID)
        .contains(&raw)
        .then_some(((raw - CLK_UART1_SRC_ID) / 4 + 1) as u32)
}

fn uart_protection(uart: u32) -> Vec<ClockMmioWriteProtection> {
    assert!((1..=9).contains(&uart));

    let mut gate_masks = [0_u32; 3];
    gate_masks[0] |= 1 << (uart + 1);
    let serial_gate_start = 11 + (uart - 1) * 3;
    for serial_gate in serial_gate_start..serial_gate_start + 3 {
        gate_masks[(serial_gate / 16) as usize] |= 1 << (serial_gate % 16);
    }

    let mut protections = Vec::with_capacity(6);
    for (register, mask) in gate_masks.into_iter().enumerate() {
        if mask == 0 {
            continue;
        }
        protections.push(ClockMmioWriteProtection::MaskedWrite32 {
            offset: clkgate_con(12 + register as u32) as usize,
            value_mask: mask,
            write_enable_mask: mask << 16,
        });
    }

    let selector_register = 41 + (uart - 1) * 2;
    let source_mask = (1 << 14) | (0x1f << 9);
    protections.extend_from_slice(&[
        ClockMmioWriteProtection::MaskedWrite32 {
            offset: clksel_con(selector_register) as usize,
            value_mask: source_mask,
            write_enable_mask: source_mask << 16,
        },
        ClockMmioWriteProtection::Deny {
            offset: clksel_con(selector_register + 1) as usize,
            length: 4,
        },
        ClockMmioWriteProtection::MaskedWrite32 {
            offset: clksel_con(selector_register + 2) as usize,
            value_mask: 0x3,
            write_enable_mask: 0x3 << 16,
        },
    ]);
    protections
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn uart2_clocks_protect_the_complete_rk3588_dependency() {
        assert_eq!(
            uart_protection(2),
            vec![
                ClockMmioWriteProtection::MaskedWrite32 {
                    offset: 0x830,
                    value_mask: 0xc008,
                    write_enable_mask: 0xc008_0000,
                },
                ClockMmioWriteProtection::MaskedWrite32 {
                    offset: 0x834,
                    value_mask: 0x1,
                    write_enable_mask: 0x0001_0000,
                },
                ClockMmioWriteProtection::MaskedWrite32 {
                    offset: 0x3ac,
                    value_mask: 0x7e00,
                    write_enable_mask: 0x7e00_0000,
                },
                ClockMmioWriteProtection::Deny {
                    offset: 0x3b0,
                    length: 4,
                },
                ClockMmioWriteProtection::MaskedWrite32 {
                    offset: 0x3b4,
                    value_mask: 0x3,
                    write_enable_mask: 0x0003_0000,
                },
            ]
        );
        assert_eq!(uart_number(ClkId::new(172)), Some(2));
        assert_eq!(uart_number(ClkId::new(187)), Some(2));
    }
}
