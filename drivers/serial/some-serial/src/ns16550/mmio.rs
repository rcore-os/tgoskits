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

/// Register behavior selected from the UART's hardware compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UartVariant {
    /// Standard NS16550 register behavior.
    Standard,
    /// XScale-compatible UART requiring IER.UUE to remain set.
    XScale,
}

/// An unsupported or unaligned MMIO register layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("MMIO UART requires 1/2/4-byte aligned accesses and a compatible register stride")]
pub struct InvalidMmioLayout;

#[derive(Clone)]
pub struct Mmio {
    base: usize,
    register_stride: usize,
    access_width: usize,
    /// Bits forced to one on IER writes while LCR.DLAB is clear.
    ier_force_set_bits: u8,
}

impl Kind for Mmio {
    fn read_reg(&self, reg: u8) -> u8 {
        let addr = self.base + usize::from(reg) * self.register_stride;
        // SAFETY: construction validates width, base and stride alignment.
        // The platform retains the mapped register window for this backend.
        unsafe {
            match self.access_width {
                1 => (addr as *const u8).read_volatile(),
                2 => (addr as *const u16).read_volatile() as u8,
                4 => (addr as *const u32).read_volatile() as u8,
                _ => unreachable!("validated MMIO access width"),
            }
        }
    }

    fn write_reg(&self, reg: u8, mut val: u8) {
        // IER aliases DLH while DLAB is set. Only enforce the unit-enable bit
        // in the interrupt bank, under the caller's existing register gate.
        if self.ier_force_set_bits != 0
            && reg == UART_IER
            && self.read_reg(UART_LCR) & DIVISOR_LATCH_ACCESS == 0
        {
            val |= self.ier_force_set_bits;
        }
        let addr = self.base + usize::from(reg) * self.register_stride;
        // SAFETY: the same validated layout and platform-owned mapping apply
        // to writes. Only the low byte carries the 16550 register value.
        unsafe {
            match self.access_width {
                1 => (addr as *mut u8).write_volatile(val),
                2 => (addr as *mut u16).write_volatile(u16::from(val)),
                4 => (addr as *mut u32).write_volatile(u32::from(val)),
                _ => unreachable!("validated MMIO access width"),
            }
        }
    }

    fn get_base(&self) -> usize {
        self.base
    }
}

impl Ns16550<Mmio> {
    /// Creates an MMIO UART with the selected hardware variant.
    /// `clock_freq` is in Hz. `register_stride` and `access_width` are separate
    /// byte counts: a UART may need byte accesses at four-byte intervals.
    /// Access width must be 1, 2 or 4; base and stride must be aligned to it,
    /// and the stride must be at least that width. Invalid layouts are rejected
    /// before accessing hardware. The platform must retain the mapped window.
    pub fn new_mmio(
        base: NonNull<u8>,
        clock_freq: u32,
        register_stride: usize,
        access_width: usize,
        variant: UartVariant,
    ) -> Result<Ns16550<Mmio>, InvalidMmioLayout> {
        if !matches!(access_width, 1 | 2 | 4)
            || register_stride < access_width
            || !register_stride.is_multiple_of(access_width)
            || !(base.as_ptr() as usize).is_multiple_of(access_width)
        {
            return Err(InvalidMmioLayout);
        }
        let base = Mmio {
            base: base.as_ptr() as usize,
            register_stride,
            access_width,
            ier_force_set_bits: match variant {
                UartVariant::Standard => 0,
                UartVariant::XScale => XSCALE_UNIT_ENABLE,
            },
        };

        Ok(Ns16550 {
            base,
            clock_freq,
            saved_lsr: super::registers::LineStatusFlags::empty(),
            tx_load_size: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use core::cell::UnsafeCell;

    use super::*;

    #[test]
    fn mmio_xscale_writes_preserve_unit_enable_and_divisor_latch() {
        let registers = UnsafeCell::new([0xa5a5_a500_u32; 8]);
        let pointer = registers.get().cast::<u32>();
        let uart = Ns16550::new_mmio(
            NonNull::new(pointer.cast()).unwrap(),
            24_000_000,
            4,
            4,
            UartVariant::XScale,
        )
        .unwrap();
        // SAFETY: the aligned array remains live and exclusive to this test;
        // UnsafeCell permits raw MMIO-style access without aliased references.
        unsafe {
            uart.base.write_reg(UART_IER, 1);
            assert_eq!(pointer.add(UART_IER as usize).read(), 0x41);
            uart.base.write_reg(UART_LCR, DIVISOR_LATCH_ACCESS);
            uart.base.write_reg(UART_IER, 0x12);
            assert_eq!(pointer.add(UART_IER as usize).read(), 0x12);
            uart.base.write_reg(UART_LCR, 0);
            uart.base.write_reg(UART_IER, 0);
            assert_eq!(pointer.add(UART_IER as usize).read(), 0x40);
        }
    }

    #[test]
    fn mmio_access_width_is_independent_of_register_stride() {
        for width in [1, 2, 4] {
            let registers = UnsafeCell::new([0xa5a5_a500_u32; 16]);
            let pointer = registers.get().cast::<u8>();
            let base = NonNull::new(pointer).unwrap();
            let uart = Ns16550::new_mmio(base, 0, 8, width, UartVariant::Standard).unwrap();
            uart.base.write_reg(UART_IER, 0x21);
            assert_eq!(uart.base.read_reg(UART_IER), 0x21);
            // SAFETY: the array is aligned, initialized, exclusively owned,
            // and retains all eight-byte register slots for the whole test.
            unsafe {
                let word = pointer.add(8).cast::<u32>().read();
                let expected = match width {
                    1 => 0xa5a5_a521,
                    2 => 0xa5a5_0021,
                    4 => 0x21,
                    _ => unreachable!(),
                };
                assert_eq!(word, expected);
                assert_eq!(pointer.add(12).cast::<u32>().read(), 0xa5a5_a500);
            }
        }

        let registers = UnsafeCell::new([0xa5a5_a500_u32; 16]);
        let pointer = registers.get().cast::<u8>();
        for (offset, stride, width) in [(0, 4, 3), (1, 4, 4), (0, 2, 4), (0, 5, 2)] {
            // SAFETY: all offsets are inside the retained register storage.
            let base = NonNull::new(unsafe { pointer.add(offset) }).unwrap();
            assert!(matches!(
                Ns16550::new_mmio(base, 0, stride, width, UartVariant::Standard),
                Err(InvalidMmioLayout)
            ));
        }
        // SAFETY: no valid backend escaped or accessed this local array.
        assert!(
            unsafe { &*registers.get() }
                .iter()
                .all(|word| *word == 0xa5a5_a500)
        );
    }
}
