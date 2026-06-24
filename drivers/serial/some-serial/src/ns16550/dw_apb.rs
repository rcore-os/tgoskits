//! Synopsys DesignWare APB UART backend for the NS16550-compatible core.

use rdif_serial::RawUart;

use super::{Config, DataBits, Kind, Ns16550, Parity, StopBits, registers::*};

/// Default UART source clock used by SG2002 / CV181x boards.
pub const SG2002_UART_CLOCK: u32 = 25_000_000;
/// Default UART source clock used by RK3588 boards.
pub const RK3588_UART_CLOCK: u32 = 24_000_000;
/// Default console baud rate.
pub const DEFAULT_BAUDRATE: u32 = 115_200;

const DLF_LEN: u32 = 6;
const REG_WIDTH: usize = 4;
const UART_USR_OFFSET: usize = 0x7c;
const UART_DLF_OFFSET: usize = 0xc0;
const UART_CPR_OFFSET: usize = 0xf4;

/// DW APB UART register backend.
///
/// The IP block is 8250/16550-compatible, but its registers are accessed as
/// 32-bit MMIO words and it exposes DesignWare extensions such as USR, DLF,
/// and CPR.
#[derive(Clone, Debug)]
pub struct DwApb {
    base: usize,
}

/// Synopsys DesignWare APB 8250-compatible UART.
pub type DwApbUart = Ns16550<DwApb>;

impl DwApb {
    /// Creates a register backend from an already-mapped MMIO base address.
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    fn reg_addr(&self, byte_offset: usize) -> usize {
        self.base + byte_offset
    }

    fn read_u32(&self, byte_offset: usize) -> u32 {
        unsafe { (self.reg_addr(byte_offset) as *const u32).read_volatile() }
    }

    fn write_u32(&self, byte_offset: usize, value: u32) {
        unsafe {
            (self.reg_addr(byte_offset) as *mut u32).write_volatile(value);
        }
    }

    fn wait_not_busy(&self) {
        while self.read_u32(UART_USR_OFFSET) & 0b1 != 0 {
            core::hint::spin_loop();
        }
    }

    fn line_status(&self) -> u8 {
        self.read_reg(UART_LSR)
    }

    fn cpr(&self) -> u32 {
        self.read_u32(UART_CPR_OFFSET)
    }
}

impl Kind for DwApb {
    fn read_reg(&self, reg: u8) -> u8 {
        (self.read_u32(reg as usize * REG_WIDTH) & 0xff) as u8
    }

    fn write_reg(&self, reg: u8, val: u8) {
        self.write_u32(reg as usize * REG_WIDTH, val as u32);
    }

    fn get_base(&self) -> usize {
        self.base
    }

    fn ack_busy_detect(&self) {
        let _ = self.read_u32(UART_USR_OFFSET);
    }

    fn set_baudrate(&self, clock_freq: u32, baudrate: u32) -> Result<(), super::ConfigError> {
        if baudrate == 0 || clock_freq == 0 {
            return Err(super::ConfigError::InvalidBaudrate);
        }

        let divider = ((clock_freq as u64) << (DLF_LEN - 4)) / baudrate as u64;
        let integer_divisor = divider >> DLF_LEN;
        if divider == 0 || integer_divisor > 0xffff {
            return Err(super::ConfigError::InvalidBaudrate);
        }

        self.wait_not_busy();

        let mut lcr: LineControlFlags = self.read_flags(UART_LCR);
        lcr.insert(LineControlFlags::DIVISOR_LATCH_ACCESS);
        self.write_flags(UART_LCR, lcr);

        self.write_reg(UART_DLL, ((divider >> DLF_LEN) & 0xff) as u8);
        self.write_reg(UART_DLH, ((divider >> (DLF_LEN + 8)) & 0xff) as u8);
        self.write_u32(UART_DLF_OFFSET, (divider & ((1 << DLF_LEN) - 1)) as u32);

        lcr.remove(LineControlFlags::DIVISOR_LATCH_ACCESS);
        self.write_flags(UART_LCR, lcr);

        Ok(())
    }

    fn baudrate(&self, clock_freq: u32) -> u32 {
        let dll = self.read_reg(UART_DLL) as u64;
        let dlh = self.read_reg(UART_DLH) as u64;
        let dlf = (self.read_u32(UART_DLF_OFFSET) & ((1 << DLF_LEN) - 1)) as u64;
        let divider = (dll << DLF_LEN) | (dlh << (DLF_LEN + 8)) | dlf;

        if divider == 0 {
            return 0;
        }

        (((clock_freq as u64) << (DLF_LEN - 4)) / divider) as u32
    }
}

impl Ns16550<DwApb> {
    /// Creates a DW APB UART with the SG2002 25 MHz default source clock.
    pub const fn new(base: usize) -> Self {
        Self::new_with_clock(base, SG2002_UART_CLOCK)
    }

    /// Creates a DW APB UART with an explicit source clock.
    pub const fn new_with_clock(base: usize, clock_freq: u32) -> Self {
        Ns16550 {
            base: DwApb::new(base),
            clock_freq,
            saved_lsr: LineStatusFlags::empty(),
        }
    }

    /// Initializes the UART at [`DEFAULT_BAUDRATE`] using its current source clock.
    pub fn init(&mut self) {
        self.init_with_baud(DEFAULT_BAUDRATE);
    }

    /// Initializes the UART at `baud` using its current source clock.
    pub fn init_with_baud(&mut self, baud: u32) {
        self.try_init_with_baud_clk(baud, self.clock_freq)
            .expect("invalid DW APB UART baud rate");
    }

    /// Initializes the UART at `baud` with an explicit source clock.
    pub fn init_with_baud_clk(&mut self, baud: u32, clk_hz: u32) {
        self.try_init_with_baud_clk(baud, clk_hz)
            .expect("invalid DW APB UART baud rate");
    }

    /// Initializes the UART at `baud` with an explicit source clock.
    pub fn try_init_with_baud_clk(
        &mut self,
        baud: u32,
        clk_hz: u32,
    ) -> Result<(), super::ConfigError> {
        self.clock_freq = clk_hz;

        self.base.write_reg(UART_IER, 0);
        self.base.write_reg(UART_FCR, UART_FCR_ENABLE_FIFO);
        self.base
            .write_reg(UART_MCR, UART_MCR_DTR | UART_MCR_RTS | UART_MCR_OUT2);

        self.set_config(
            &Config::new()
                .baudrate(baud)
                .data_bits(DataBits::Eight)
                .stop_bits(StopBits::One)
                .parity(Parity::None),
        )
    }

    /// Initializes the UART with an explicit source clock and baud rate.
    pub fn ns16550_init(&mut self, clk_hz: u32, baud: u32) {
        self.init_with_baud_clk(baud, clk_hz);
    }

    /// Reads the line status register.
    pub fn line_status(&self) -> u32 {
        self.base.line_status() as u32
    }

    /// Reads the component parameter register.
    pub fn cpr(&self) -> u32 {
        self.base.cpr()
    }

    pub fn new_raw(base: core::ptr::NonNull<u8>, clock_freq: u32) -> Self {
        Self::new_with_clock(base.as_ptr() as usize, clock_freq)
    }
}

#[cfg(test)]
mod tests {
    use std::boxed::Box;

    use super::*;

    #[test]
    fn busy_detect_interrupt_is_claimed_as_irq_ack() {
        let regs = Box::leak(Box::new([0u32; 0x100 / 4]));
        regs[UART_IIR as usize] = UART_IIR_BUSY as u32;
        regs[UART_USR_OFFSET / 4] = 0x1;

        let mut uart = DwApbUart::new(regs.as_ptr() as usize);

        assert_eq!(uart.handle_irq(), rdif_serial::SerialEvent::IRQ_ACK);
        assert_eq!(regs[UART_USR_OFFSET / 4], 0x1);
    }

    #[test]
    fn new_raw_does_not_touch_hardware_registers() {
        let regs = Box::leak(Box::new([0u32; 0x100 / 4]));
        regs[UART_DLF_OFFSET / 4] = 0x33;

        let base = core::ptr::NonNull::new(regs.as_mut_ptr().cast()).unwrap();
        let serial = DwApbUart::new_raw(base, SG2002_UART_CLOCK);

        assert_eq!(regs[UART_FCR as usize], 0);
        assert_eq!(regs[UART_MCR as usize], 0);
        assert_eq!(regs[UART_DLF_OFFSET / 4], 0x33);
        drop(serial);
    }
}
