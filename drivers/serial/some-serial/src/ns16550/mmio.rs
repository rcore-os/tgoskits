//! NS16550 MMIO 版本实现
//!
//! 适用于嵌入式平台的内存映射 IO 版本

use core::ptr::NonNull;

use super::{
    Kind, Ns16550,
    registers::{InterruptEnableFlags, UART_IER, UART_LCR, UART_MSR},
};

const XSCALE_UNIT_ENABLE: u8 = 1 << 6;
const XSCALE_RX_TIMEOUT_ENABLE: u8 = 1 << 4;
const DIVISOR_LATCH_ACCESS: u8 = 1 << 7;

/// Register behavior selected from the UART's hardware compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UartVariant {
    /// Standard NS16550 register behavior.
    Standard,
    /// XScale-compatible UART with unit enable and separate RX timeout enable.
    XScale,
}

impl UartVariant {
    /// Resolves the MMIO register stride and access width in bytes.
    ///
    /// An unspecified width defaults to one byte. Without an explicit shift,
    /// XScale uses four-byte spacing and standard UARTs use the access width
    /// as the stride.
    pub fn mmio_layout(
        self,
        register_shift: Option<u32>,
        access_width: Option<u32>,
    ) -> Result<MmioLayout, InvalidMmioLayout> {
        let access_width = access_width.unwrap_or(1) as usize;
        let register_stride = match register_shift {
            Some(shift) => 1usize.checked_shl(shift).ok_or(InvalidMmioLayout)?,
            None if self == Self::XScale => 4,
            None => access_width,
        };
        MmioLayout::new(register_stride, access_width)
    }
}

/// An unsupported, unaligned or out-of-range MMIO register layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("MMIO UART registers must fit the mapped range with aligned 1/2/4-byte accesses")]
pub struct InvalidMmioLayout;

/// Validated spacing and access width of the memory-mapped UART registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MmioLayout {
    register_stride: usize,
    access_width: usize,
    required_len: usize,
}

impl MmioLayout {
    /// Creates a layout from byte counts. Spacing must be a multiple of an
    /// access width of one, two, or four bytes and cover at least that width.
    pub fn new(register_stride: usize, access_width: usize) -> Result<Self, InvalidMmioLayout> {
        if !matches!(access_width, 1 | 2 | 4)
            || register_stride < access_width
            || !register_stride.is_multiple_of(access_width)
        {
            return Err(InvalidMmioLayout);
        }

        let required_len = usize::from(UART_MSR)
            .checked_mul(register_stride)
            .and_then(|offset| offset.checked_add(access_width))
            .ok_or(InvalidMmioLayout)?;
        Ok(Self {
            register_stride,
            access_width,
            required_len,
        })
    }

    /// Minimum number of mapped bytes, starting from the first register.
    pub const fn required_len(self) -> usize {
        self.required_len
    }
}

#[derive(Clone)]
pub struct Mmio {
    base: usize,
    layout: MmioLayout,
    variant: UartVariant,
}

impl Mmio {
    fn register_address(&self, reg: u8) -> usize {
        assert!(reg <= UART_MSR, "register outside the NS16550 MMIO layout");
        self.base + usize::from(reg) * self.layout.register_stride
    }
}

impl Kind for Mmio {
    fn read_reg(&self, reg: u8) -> u8 {
        let addr = self.register_address(reg);
        // SAFETY: the register index and layout keep the aligned access in the
        // register window retained by the platform under `new_mmio`'s contract.
        unsafe {
            match self.layout.access_width {
                1 => (addr as *const u8).read_volatile(),
                2 => (addr as *const u16).read_volatile() as u8,
                4 => (addr as *const u32).read_volatile() as u8,
                _ => unreachable!("validated MMIO access width"),
            }
        }
    }

    fn write_reg(&self, reg: u8, mut val: u8) {
        // IER aliases DLH while DLAB is set. Apply XScale's IRQ bits only
        // in the interrupt bank, under the caller's existing register gate.
        if self.variant == UartVariant::XScale
            && reg == UART_IER
            && self.read_reg(UART_LCR) & DIVISOR_LATCH_ACCESS == 0
        {
            val |= XSCALE_UNIT_ENABLE;
            // A short RX burst below the FIFO threshold needs a timeout IRQ.
            // Mask it with RX so deferred service cannot be interrupted again.
            if val & InterruptEnableFlags::RECEIVED_DATA_AVAILABLE.bits() != 0 {
                val |= XSCALE_RX_TIMEOUT_ENABLE;
            } else {
                val &= !XSCALE_RX_TIMEOUT_ENABLE;
            }
        }
        let addr = self.register_address(reg);
        // SAFETY: the register index and layout keep the aligned access in the
        // register window retained by the platform under `new_mmio`'s contract.
        unsafe {
            match self.layout.access_width {
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
    /// `clock_freq` is in Hz. The base must be aligned to the layout's access
    /// width, and the register span must fit in `mapped_len` bytes at that base.
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
        layout: MmioLayout,
        variant: UartVariant,
    ) -> Result<Ns16550<Mmio>, InvalidMmioLayout> {
        let required_len = layout.required_len();
        let base = base.as_ptr() as usize;
        if !base.is_multiple_of(layout.access_width)
            || required_len > mapped_len
            || base.checked_add(required_len).is_none()
        {
            return Err(InvalidMmioLayout);
        }
        let base = Mmio {
            base,
            layout,
            variant,
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

    use rdif_serial::{Config, ConfigError, SerialEventSet, UartPort};

    use super::*;

    #[test]
    fn mmio_xscale_keeps_unit_enabled_and_masks_timeout_with_rx() {
        let registers = UnsafeCell::new([0xa5a5_a500_u32; 8]);
        let pointer = registers.get().cast::<u32>();
        // SAFETY: the aligned array remains live and exclusive to this test.
        let mut uart = unsafe {
            Ns16550::new_mmio(
                NonNull::new(pointer.cast()).unwrap(),
                core::mem::size_of_val(&registers),
                0,
                MmioLayout::new(4, 4).unwrap(),
                UartVariant::XScale,
            )
        }
        .unwrap();
        // SAFETY: the aligned array remains live and exclusive to this test;
        // UnsafeCell permits raw MMIO-style access without aliased references.
        unsafe {
            assert!(
                uart.rearm(SerialEventSet::RX | SerialEventSet::TX_SPACE)
                    .is_empty()
            );
            assert_eq!(pointer.add(UART_IER as usize).read(), 0x57);
            uart.mask(SerialEventSet::RX);
            assert_eq!(pointer.add(UART_IER as usize).read(), 0x42);
            uart.mask_all();
            assert_eq!(pointer.add(UART_IER as usize).read(), 0x40);
            uart.base.write_reg(UART_LCR, DIVISOR_LATCH_ACCESS);
            uart.base.write_reg(UART_IER, 0x12);
            assert_eq!(pointer.add(UART_IER as usize).read(), 0x12);
            uart.base.write_reg(UART_LCR, 0);
            uart.base.write_reg(UART_IER, 0);
            assert_eq!(pointer.add(UART_IER as usize).read(), 0x40);

            pointer.add(UART_IER as usize).write(0);
            assert_eq!(
                uart.startup(&Config::new().baudrate(115_200)),
                Err(ConfigError::BaudrateUnavailable)
            );
            assert_eq!(pointer.add(UART_IER as usize).read(), 0);
        }
    }

    #[test]
    fn mmio_access_width_is_independent_of_register_stride() {
        for width in [1, 2, 4] {
            let registers = UnsafeCell::new([0xa5a5_a500_u32; 16]);
            let pointer = registers.get().cast::<u8>();
            let base = NonNull::new(pointer).unwrap();
            let layout = MmioLayout::new(8, width).unwrap();
            assert_eq!(layout.required_len(), 48 + width);
            // SAFETY: the array retains the complete register span for the
            // duration of this test and is accessed only on this thread.
            let uart = unsafe {
                Ns16550::new_mmio(
                    base,
                    layout.required_len(),
                    0,
                    layout,
                    UartVariant::Standard,
                )
            }
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
        for (stride, width) in [(4, 3), (2, 4), (5, 2), (usize::MAX / 2 + 1, 1)] {
            assert_eq!(MmioLayout::new(stride, width), Err(InvalidMmioLayout));
        }

        for (offset, len, stride) in [(1, 63, 4), (0, 51, 8)] {
            // SAFETY: both offsets are inside the retained register storage.
            let base = NonNull::new(unsafe { pointer.add(offset) }).unwrap();
            let layout = MmioLayout::new(stride, 4).unwrap();
            // SAFETY: both mappings are rejected before accessing registers.
            let result = unsafe { Ns16550::new_mmio(base, len, 0, layout, UartVariant::Standard) };
            assert!(
                matches!(result, Err(InvalidMmioLayout)),
                "accepted offset {offset}, length {len}, stride {stride}"
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
