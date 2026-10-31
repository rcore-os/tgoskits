//! NS16550 MMIO 版本实现
//!
//! 适用于嵌入式平台的内存映射 IO 版本

use core::ptr::NonNull;

use super::{
    Kind, Ns16550,
    registers::{UART_IER, UART_LCR},
};

const XSCALE_UNIT_ENABLE: u8 = 1 << 6;
const DIVISOR_LATCH_ACCESS: u8 = 1 << 7;

#[derive(Clone)]
pub struct Mmio {
    base: usize,
    width: usize,
    ier_fixed_bits: u8,
}

impl Kind for Mmio {
    fn read_reg(&self, reg: u8) -> u8 {
        unsafe {
            let addr = self.base + (reg as usize) * self.width;
            (addr as *const u8).read_volatile()
        }
    }

    fn write_reg(&self, reg: u8, mut val: u8) {
        // IER aliases DLH while DLAB is set. Only enforce the unit-enable bit
        // in the interrupt bank, under the caller's existing register gate.
        if self.ier_fixed_bits != 0
            && reg == UART_IER
            && self.read_reg(UART_LCR) & DIVISOR_LATCH_ACCESS == 0
        {
            val |= self.ier_fixed_bits;
        }
        unsafe {
            let addr = self.base + (reg as usize) * self.width;
            (addr as *mut u8).write_volatile(val);
        }
    }

    fn get_base(&self) -> usize {
        self.base
    }
}

impl Ns16550<Mmio> {
    /// Creates an XScale-compatible UART whose IER unit-enable bit must stay set.
    pub fn new_xscale_mmio(base: NonNull<u8>, clock_freq: u32, reg_width: usize) -> Ns16550<Mmio> {
        let mut uart = Self::new_mmio(base, clock_freq, reg_width);
        uart.base.ier_fixed_bits = XSCALE_UNIT_ENABLE;
        uart
    }

    pub fn new_mmio(base: NonNull<u8>, clock_freq: u32, reg_width: usize) -> Ns16550<Mmio> {
        let base = Mmio {
            base: base.as_ptr() as usize,
            width: reg_width,
            ier_fixed_bits: 0,
        };

        Ns16550 {
            base,
            clock_freq,
            saved_lsr: super::registers::LineStatusFlags::empty(),
            tx_load_size: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use rdif_serial::{SerialEventSet, UartPort};

    use super::*;
    use crate::ns16550::registers::{UART_DLH, UART_IER, UART_LCR};

    #[test]
    fn xscale_masking_preserves_unit_enable_without_changing_divisor_latch() {
        let mut registers = [0_u8; 8];
        let base = NonNull::new(registers.as_mut_ptr()).unwrap();
        let mut uart = Ns16550::new_xscale_mmio(base, 24_000_000, 1);
        uart.base.write_reg(UART_IER, 0x4f);
        uart.mask_all();
        assert_eq!(uart.base.read_reg(UART_IER), 0x40);
        uart.rearm(SerialEventSet::RX_DATA);
        assert_ne!(uart.base.read_reg(UART_IER) & 0x40, 0);
        uart.base.write_reg(UART_LCR, 0x80);
        uart.base.write_reg(UART_DLH, 0x03);
        assert_eq!(uart.base.read_reg(UART_DLH), 0x03);
    }
}
