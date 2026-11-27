//! NS16550 MMIO 版本实现
//!
//! 适用于嵌入式平台的内存映射 IO 版本

use core::ptr::NonNull;

use super::{
    Kind, Ns16550,
    registers::{UART_IER, UART_IER_RDI, UART_LCR, UART_SCR},
};

const XSCALE_RECEIVE_TIMEOUT_ENABLE: u8 = 1 << 4;
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

/// An MMIO register layout that cannot be accessed within its mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("MMIO UART requires aligned 1/2/4-byte accesses within the mapped register window")]
pub struct InvalidMmioLayout;

#[derive(Clone)]
pub struct Mmio {
    base: usize,
    register_stride: usize,
    access_width: usize,
    variant: UartVariant,
}

impl Mmio {
    fn register_addr(&self, reg: u8) -> usize {
        assert!(reg <= UART_SCR);
        self.base + usize::from(reg) * self.register_stride
    }
}

impl Kind for Mmio {
    fn read_reg(&self, reg: u8) -> u8 {
        let addr = self.register_addr(reg);
        // SAFETY: the checked constructor validates every register against the
        // retained mapping. Legacy callers supply a byte-accessible UART mapping.
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
        // IER aliases DLH while DLAB is set. Apply the XScale IER bits only in
        // the interrupt bank, under the caller's existing register gate.
        if self.variant == UartVariant::XScale
            && reg == UART_IER
            && self.read_reg(UART_LCR) & DIVISOR_LATCH_ACCESS == 0
        {
            val |= XSCALE_UNIT_ENABLE;
            // Mask character timeout together with the rest of the RX source.
            val &= !XSCALE_RECEIVE_TIMEOUT_ENABLE;
            if val & UART_IER_RDI != 0 {
                val |= XSCALE_RECEIVE_TIMEOUT_ENABLE;
            }
        }
        let addr = self.register_addr(reg);
        // SAFETY: the same retained mapping and constructor-established alignment
        // apply to writes. Only the low byte contains the UART register value.
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
    /// Creates a standard NS16550 with byte accesses and a byte register stride.
    pub fn new_mmio(base: NonNull<u8>, clock_freq: u32, reg_width: usize) -> Ns16550<Mmio> {
        Ns16550 {
            base: Mmio {
                base: base.as_ptr() as usize,
                register_stride: reg_width,
                access_width: 1,
                variant: UartVariant::Standard,
            },
            clock_freq,
            saved_lsr: super::registers::LineStatusFlags::empty(),
            tx_load_size: 1,
        }
    }

    /// Creates an MMIO UART with variant-specific register behavior.
    /// `clock_freq` is in Hz. Access width must be 1, 2 or 4 bytes, with the
    /// base and stride aligned to it. The entire register bank must fit in `window`.
    ///
    /// # Safety
    ///
    /// `window` must describe a UART MMIO mapping that remains valid for the
    /// lifetime of the UART and its split endpoints. If the layout is accepted,
    /// the device must support volatile accesses with that width and stride.
    pub unsafe fn new_mmio_with_variant(
        window: NonNull<[u8]>,
        clock_freq: u32,
        register_stride: usize,
        access_width: usize,
        variant: UartVariant,
    ) -> Result<Ns16550<Mmio>, InvalidMmioLayout> {
        let base = window.cast::<u8>();
        if !matches!(access_width, 1 | 2 | 4)
            || register_stride < access_width
            || !register_stride.is_multiple_of(access_width)
            || !(base.as_ptr() as usize).is_multiple_of(access_width)
        {
            return Err(InvalidMmioLayout);
        }
        let required_len = usize::from(UART_SCR)
            .checked_mul(register_stride)
            .and_then(|offset| offset.checked_add(access_width))
            .filter(|&length| length <= window.len())
            .ok_or(InvalidMmioLayout)?;
        (base.as_ptr() as usize)
            .checked_add(required_len - 1)
            .ok_or(InvalidMmioLayout)?;
        let mut uart = Self::new_mmio(base, clock_freq, register_stride);
        uart.base.access_width = access_width;
        uart.base.variant = variant;
        Ok(uart)
    }
}

#[cfg(test)]
mod tests {
    use core::cell::UnsafeCell;

    use rdif_serial::{SerialEventSet, SplitUart, UartIrq, UartPort, UartRegisterGate};

    use super::{
        super::registers::{UART_IER_RLSI, UART_THR},
        *,
    };

    #[test]
    fn mmio_console_takeover_respects_variant_and_divisor_bank() {
        for access_width in [1, 2, 4] {
            for variant in [UartVariant::Standard, UartVariant::XScale] {
                // Eight-byte register slots keep stride independent of access width.
                // Nonzero upper bytes make an incorrectly narrow write observable.
                let mut initial = [0xa5a5_a500_u32; 16];
                initial[2] |= 0xff;
                initial[4] |= 0xc0;
                initial[10] |= 0x20;
                let registers = UnsafeCell::new(initial);
                let pointer = registers.get().cast::<u32>();
                let base = NonNull::new(pointer.cast::<u8>()).unwrap();
                let window = NonNull::slice_from_raw_parts(base, size_of_val(&initial));
                // SAFETY: aligned exclusive test storage outlives all endpoints.
                let uart = unsafe {
                    Ns16550::new_mmio_with_variant(window, 24_000_000, 8, access_width, variant)
                }
                .unwrap();
                let expected = if variant == UartVariant::XScale {
                    XSCALE_UNIT_ENABLE
                } else {
                    0
                };
                let untouched = match access_width {
                    1 => 0xa5a5_a500,
                    2 => 0xa5a5_0000,
                    4 => 0,
                    _ => unreachable!(),
                };
                uart.base.write_reg(UART_LCR, DIVISOR_LATCH_ACCESS);
                uart.base.write_reg(UART_IER, 0x12);
                assert_eq!(uart.base.read_reg(UART_IER), 0x12);
                // SAFETY: aligned initialized storage remains live and exclusively
                // owned by this test; UnsafeCell permits the volatile UART writes.
                assert_eq!(unsafe { pointer.add(2).read() }, untouched | 0x12);
                uart.base.write_reg(UART_LCR, 0);
                uart.base.write_reg(UART_IER, 0xff);
                let mut parts = uart.split();
                parts.control.mask_all();
                assert_eq!(parts.control.base.read_reg(UART_IER), expected);
                let rx_enabled = expected
                    | UART_IER_RDI
                    | UART_IER_RLSI
                    | if variant == UartVariant::XScale {
                        1 << 4
                    } else {
                        0
                    };
                assert!(parts.control.rearm(SerialEventSet::RX).is_empty());
                assert_eq!(parts.control.base.read_reg(UART_IER), rx_enabled);
                parts.irq.mask(SerialEventSet::RX);
                assert_eq!(parts.control.base.read_reg(UART_IER), expected);
                assert!(parts.control.rearm(SerialEventSet::RX).is_empty());
                parts.control.mask(SerialEventSet::RX);
                assert_eq!(parts.control.base.read_reg(UART_IER), expected);
                assert_eq!(parts.control.write_tx(b"x"), 1);
                assert_eq!(parts.control.base.read_reg(UART_THR), b'x');
                parts.control.base.write_reg(UART_IER, 0xff);
                let gate = UartRegisterGate::new(parts.emergency_tx);
                let access = gate.try_begin_emergency().unwrap();
                assert_eq!(access.try_write(b"y"), 1);
                assert_eq!(parts.control.base.read_reg(UART_IER), expected);
                // SAFETY: the same retained, exclusively owned aligned window
                // also contains THR and the word between adjacent register slots.
                unsafe {
                    assert_eq!(pointer.read(), untouched | u32::from(b'y'));
                    assert_eq!(pointer.add(1).read(), initial[1]);
                }
            }
        }

        let registers = UnsafeCell::new([0xa5a5_a500_u32; 16]);
        let pointer = registers.get().cast::<u8>();
        for (offset, stride, width, length) in [
            (0, 4, 3, 64),
            (1, 4, 4, 63),
            (0, 2, 4, 64),
            (0, 5, 2, 64),
            (0, 0, 1, 64),
            (0, 8, 4, 59),
            (0, 1usize << (usize::BITS - 1), 1, 64),
        ] {
            // SAFETY: both candidate addresses remain within the live array.
            let base = NonNull::new(unsafe { pointer.add(offset) }).unwrap();
            let window = NonNull::slice_from_raw_parts(base, length);
            // SAFETY: the entire window is inside the live storage, with no IO
            // performed until the constructor has validated the layout.
            let result = unsafe {
                Ns16550::new_mmio_with_variant(window, 0, stride, width, UartVariant::Standard)
            };
            assert!(
                matches!(result, Err(InvalidMmioLayout)),
                "stride={stride}, width={width}, length={length}"
            );
        }
        // SAFETY: no rejected constructor accessed or retained the array.
        assert!(
            unsafe { &*registers.get() }
                .iter()
                .all(|word| *word == 0xa5a5_a500)
        );
    }
}
