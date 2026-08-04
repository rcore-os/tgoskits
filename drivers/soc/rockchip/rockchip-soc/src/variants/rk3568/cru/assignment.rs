//! RK3568 host-owned UART clock register protection.

use alloc::{vec, vec::Vec};

use crate::{
    ClkId, ClockAssignmentProtection, ClockMmioWriteProtection,
    variants::rk3568::cru::{CLKGATE_CON_OFFSET, CLKSEL_CON_OFFSET, Cru},
};

const PCLK_UART1_ID: u64 = 284;
const SCLK_UART9_ID: u64 = 319;

impl ClockAssignmentProtection for Cru {
    fn assignment_mmio_write_protection(&self, id: ClkId) -> Option<Vec<ClockMmioWriteProtection>> {
        let uart = uart_number(id)?;
        Some(uart_protection(uart))
    }
}

fn uart_number(id: ClkId) -> Option<u32> {
    let raw = id.value();
    (PCLK_UART1_ID..=SCLK_UART9_ID)
        .contains(&raw)
        .then_some(((raw - PCLK_UART1_ID) / 4 + 1) as u32)
}

fn uart_protection(uart: u32) -> Vec<ClockMmioWriteProtection> {
    let (gate_register, gate_shift) = match uart {
        1 => (27, 12),
        2..=5 => (28, (uart - 2) * 4),
        6..=9 => (29, (uart - 6) * 4),
        _ => unreachable!("validated RK3568 UART number"),
    };
    let gate_mask = 0xf << gate_shift;
    let selector_register = 52 + (uart - 1) * 2;
    let selector_mask = 0x7f | (0x3 << 8) | (0x3 << 12);

    vec![
        ClockMmioWriteProtection::MaskedWrite32 {
            offset: register_offset(CLKGATE_CON_OFFSET, gate_register),
            value_mask: gate_mask,
            write_enable_mask: gate_mask << 16,
        },
        ClockMmioWriteProtection::MaskedWrite32 {
            offset: register_offset(CLKSEL_CON_OFFSET, selector_register),
            value_mask: selector_mask,
            write_enable_mask: selector_mask << 16,
        },
        ClockMmioWriteProtection::Deny {
            offset: register_offset(CLKSEL_CON_OFFSET, selector_register + 1),
            length: 4,
        },
    ]
}

const fn register_offset(base: u32, index: u32) -> usize {
    (base + index * 4) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart2_clocks_protect_the_complete_rk3568_dependency() {
        let pclk = uart_protection(2);

        assert_eq!(
            pclk,
            vec![
                ClockMmioWriteProtection::MaskedWrite32 {
                    offset: 0x370,
                    value_mask: 0xf,
                    write_enable_mask: 0x000f_0000,
                },
                ClockMmioWriteProtection::MaskedWrite32 {
                    offset: 0x1d8,
                    value_mask: 0x337f,
                    write_enable_mask: 0x337f_0000,
                },
                ClockMmioWriteProtection::Deny {
                    offset: 0x1dc,
                    length: 4,
                },
            ]
        );
        assert_eq!(uart_number(ClkId::new(288)), Some(2));
        assert_eq!(uart_number(ClkId::new(291)), Some(2));
    }
}
