//! Definitions for snps,dw-apb-uart serial driver.
//! Uart snps,dw-apb-uart driver in Rust for BST A1000b FADA board.
#![no_std]

use tock_registers::{
    interfaces::{Readable, Writeable},
    register_structs,
    registers::{ReadOnly, ReadWrite},
};

register_structs! {
    DW8250Regs {
        /// Get or Put Register.
        (0x00 => rbr: ReadWrite<u32>),
        (0x04 => ier: ReadWrite<u32>),
        (0x08 => fcr: ReadWrite<u32>),
        (0x0c => lcr: ReadWrite<u32>),
        (0x10 => mcr: ReadWrite<u32>),
        (0x14 => lsr: ReadOnly<u32>),
        (0x18 => msr: ReadOnly<u32>),
        (0x1c => scr: ReadWrite<u32>),
        (0x20 => lpdll: ReadWrite<u32>),
        (0x24 => _reserved0),
        /// Uart Status Register.
        (0x7c => usr: ReadOnly<u32>),
        (0x80 => _reserved1),
        (0xc0 => dlf: ReadWrite<u32>),
        (0xc4 => @END),
    }
}

/// dw-apb-uart serial driver: DW8250
pub struct DW8250 {
    base_vaddr: usize,
}

impl DW8250 {
    /// New a DW8250
    pub const fn new(base_vaddr: usize) -> Self {
        Self { base_vaddr }
    }

    const fn regs(&self) -> &DW8250Regs {
        unsafe { &*(self.base_vaddr as *const _) }
    }

    /// DW8250 initialize
    pub fn init(&mut self) {
        self.init_with_baud_clk(115200, 25_000_000);
    }

    /// DW8250 initialize with explicit baud rate and UART source clock (Hz).
    ///
    /// The DLF fractional field is 6 bits wide (BST_UART_DLF_LEN = 6).
    /// divisor = (clk_hz << (DLF_LEN - 4)) / baud
    ///   DLL   = divisor >> DLF_LEN          (bits [13:6])
    ///   DLH   = divisor >> (DLF_LEN + 8)    (bits [21:14])
    ///   DLF   = divisor & ((1<<DLF_LEN)-1)  (bits [5:0])
    pub fn init_with_baud_clk(&mut self, baud: u32, clk_hz: u32) {
        const BST_UART_DLF_LEN: u32 = 6;
        let divider = (clk_hz << (BST_UART_DLF_LEN - 4)) / baud;

        // Waiting to be no USR_BUSY.
        while self.regs().usr.get() & 0b1 != 0 {}

        /* Disable interrupts and Enable FIFOs */
        self.regs().ier.set(0);
        self.regs().fcr.set(1);

        /* Disable flow ctrl */
        self.regs().mcr.set(0);

        /* Clear MCR_RTS */
        self.regs().mcr.set(self.regs().mcr.get() | (1 << 1));

        /* Enable access DLL & DLH. Set LCR_DLAB */
        self.regs().lcr.set(self.regs().lcr.get() | (1 << 7));

        /* Set baud rate. Set DLL, DLH, DLF */
        self.regs().rbr.set((divider >> BST_UART_DLF_LEN) & 0xff);
        self.regs()
            .ier
            .set((divider >> (BST_UART_DLF_LEN + 8)) & 0xff);
        self.regs().dlf.set(divider & ((1 << BST_UART_DLF_LEN) - 1));

        /* Clear DLAB bit */
        self.regs().lcr.set(self.regs().lcr.get() & !(1 << 7));

        /* Set data length to 8 bit, 1 stop bit, no parity. Set LCR_WLS1 | LCR_WLS0 */
        self.regs().lcr.set(self.regs().lcr.get() | 0b11);
    }

    /// Initialize with baud rate, using the RK3588 default UART source clock (24 MHz).
    pub fn init_with_baud(&mut self, baud: u32) {
        // RK3588 UART source clock: 24 MHz (xin24m crystal)
        self.init_with_baud_clk(baud, 24_000_000);
    }

    /// DW8250 serial output
    pub fn putchar(&mut self, c: u8) {
        // Check LSR_TEMT
        // Wait for last character to go.
        while self.regs().lsr.get() & (1 << 6) == 0 {}
        self.regs().rbr.set(c as u32);
    }

    /// DW8250 serial input
    pub fn getchar(&mut self) -> Option<u8> {
        // Check LSR_DR
        // Wait for a character to arrive.
        if self.regs().lsr.get() & 0b1 != 0 {
            Some((self.regs().rbr.get() & 0xff) as u8)
        } else {
            None
        }
    }

    /// DW8250 serial interrupt enable or disable
    pub fn set_ier(&mut self, enable: bool) {
        if enable {
            // Enable interrupts
            self.regs().ier.set(1);
        } else {
            // Disable interrupts
            self.regs().ier.set(0);
        }
    }
}
