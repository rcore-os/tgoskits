//! NS16550 MMIO 版本实现
//!
//! 适用于嵌入式平台的内存映射 IO 版本

use core::ptr::NonNull;

use super::{
    Kind, Ns16550,
    registers::{UART_IER, UART_LCR, UART_MSR},
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

/// An unsupported, unaligned or out-of-range MMIO register layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("MMIO UART registers must fit the mapped range with aligned 1/2/4-byte accesses")]
pub struct InvalidMmioLayout;

#[derive(Clone)]
pub struct Mmio {
    base: usize,
    register_stride: usize,
    access_width: usize,
    /// Bits forced to one on IER writes while LCR.DLAB is clear.
    ier_force_set_bits: u8,
}

impl Mmio {
    /// Returns the bytes needed to access all registers used by the driver.
    pub fn required_len(
        register_stride: usize,
        access_width: usize,
    ) -> Result<usize, InvalidMmioLayout> {
        if !matches!(access_width, 1 | 2 | 4)
            || register_stride < access_width
            || !register_stride.is_multiple_of(access_width)
        {
            return Err(InvalidMmioLayout);
        }

        usize::from(UART_MSR)
            .checked_mul(register_stride)
            .and_then(|offset| offset.checked_add(access_width))
            .ok_or(InvalidMmioLayout)
    }

    fn register_address(&self, reg: u8) -> usize {
        assert!(reg <= UART_MSR, "register outside the NS16550 MMIO layout");
        self.base + usize::from(reg) * self.register_stride
    }
}

impl Kind for Mmio {
    fn read_reg(&self, reg: u8) -> u8 {
        let addr = self.register_address(reg);
        // SAFETY: the register index and layout keep the aligned access in the
        // register window retained by the platform under `new_mmio`'s contract.
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
        let addr = self.register_address(reg);
        // SAFETY: the register index and layout keep the aligned access in the
        // register window retained by the platform under `new_mmio`'s contract.
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
    /// and the stride must be at least that width. The register window must fit
    /// within `mapped_len` bytes starting at `base`.
    ///
    /// # Safety
    /// If construction succeeds, the validated register span must remain mapped
    /// to this UART and allow volatile reads and writes of the specified width
    /// as long as the UART or any of its split endpoints is used. The caller
    /// must coordinate access across endpoints and other users of the registers.
    pub unsafe fn new_mmio(
        base: NonNull<u8>,
        mapped_len: usize,
        clock_freq: u32,
        register_stride: usize,
        access_width: usize,
        variant: UartVariant,
    ) -> Result<Ns16550<Mmio>, InvalidMmioLayout> {
        let required_len = Mmio::required_len(register_stride, access_width)?;
        let base = base.as_ptr() as usize;
        if !base.is_multiple_of(access_width)
            || required_len > mapped_len
            || base.checked_add(required_len).is_none()
        {
            return Err(InvalidMmioLayout);
        }
        let base = Mmio {
            base,
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
        // SAFETY: the aligned array remains live and exclusive to this test.
        let uart = unsafe {
            Ns16550::new_mmio(
                NonNull::new(pointer.cast()).unwrap(),
                core::mem::size_of_val(&registers),
                24_000_000,
                4,
                4,
                UartVariant::XScale,
            )
        }
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
            // SAFETY: the array retains the complete register span for the
            // duration of this test and is accessed only on this thread.
            let uart =
                unsafe { Ns16550::new_mmio(base, 48 + width, 0, 8, width, UartVariant::Standard) }
                    .unwrap();
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
        for (offset, len, stride, width) in [
            (0, 64, 4, 3),
            (1, 63, 4, 4),
            (0, 64, 2, 4),
            (0, 64, 5, 2),
            (0, 51, 8, 4),
            (0, 64, usize::MAX / 2 + 1, 1),
        ] {
            // SAFETY: all offsets are inside the retained register storage.
            let base = NonNull::new(unsafe { pointer.add(offset) }).unwrap();
            // SAFETY: all inputs are rejected before accessing the registers.
            let result =
                unsafe { Ns16550::new_mmio(base, len, 0, stride, width, UartVariant::Standard) };
            assert!(
                matches!(result, Err(InvalidMmioLayout)),
                "accepted offset {offset}, length {len}, stride {stride}, width {width}"
            );
        }
        // SAFETY: no valid backend escaped or accessed this local array.
        assert!(
            unsafe { &*registers.get() }
                .iter()
                .all(|word| *word == 0xa5a5_a500)
        );
    }
}
