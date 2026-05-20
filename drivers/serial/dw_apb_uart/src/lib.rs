//! Definitions for snps,dw-apb-uart serial driver.
//!
//! Originally written for the BST A1000b FADA board (SG2002 / CV181x,
//! 25 MHz UART source clock). Generalized so the same driver also
//! works on RK3588 boards (24 MHz xin24m crystal) by selecting one of
//! the `sg2002` / `rk3588` cargo features, or by calling
//! [`DW8250::init_with_baud_clk`] directly with an explicit clock.
#![no_std]

use tock_registers::{
    interfaces::{Readable, Writeable},
    register_structs,
    registers::{ReadOnly, ReadWrite},
};

#[cfg(all(feature = "sg2002", feature = "rk3588"))]
compile_error!("dw_apb_uart: features `sg2002` and `rk3588` are mutually exclusive; pick one");

/// Default UART source clock used by [`DW8250::init`] and
/// [`DW8250::init_with_baud`] when no explicit clock is given.
#[cfg(feature = "rk3588")]
const DEFAULT_UART_SRC_CLK: u32 = 24_000_000;
#[cfg(not(feature = "rk3588"))]
const DEFAULT_UART_SRC_CLK: u32 = 25_000_000;

/// DLF (fractional divisor latch) bit width on this controller.
const BST_UART_DLF_LEN: u32 = 6;

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
        (0xc4 => _reserved2),
        (0xf4 => cpr: ReadWrite<u32>),
        (0xf8 => @END),
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

    /// Initialize at 115200 baud using the feature-selected default clock.
    pub fn init(&mut self) {
        self.init_with_baud(115200);
    }

    /// Initialize at `baud` using the feature-selected default clock
    /// (25 MHz on sg2002, 24 MHz on rk3588).
    pub fn init_with_baud(&mut self, baud: u32) {
        self.init_with_baud_clk(baud, DEFAULT_UART_SRC_CLK);
    }

    /// Initialize at `baud` with an explicit `clk_hz` UART source clock.
    ///
    /// The 6-bit DLF fractional field gives sub-baud-quantum precision,
    /// so even high speeds like 1.5 Mbps end up within UART tolerance.
    /// Layout per the snps DW APB UART manual:
    ///   divisor = (clk_hz << (DLF_LEN - 4)) / baud
    ///     DLL = (divisor >> DLF_LEN)        & 0xff
    ///     DLH = (divisor >> (DLF_LEN + 8))  & 0xff
    ///     DLF =  divisor & ((1 << DLF_LEN) - 1)
    pub fn init_with_baud_clk(&mut self, baud: u32, clk_hz: u32) {
        let divider = (clk_hz << (BST_UART_DLF_LEN - 4)) / baud;

        // Wait until the controller is no longer busy.
        while self.regs().usr.get() & 0b1 != 0 {}

        // Disable interrupts and enable FIFOs.
        self.regs().ier.set(0);
        self.regs().fcr.set(1);

        // Disable flow control / clear MCR_RTS.
        self.regs().mcr.set(0);
        self.regs().mcr.set(self.regs().mcr.get() | (1 << 1));

        // Enable access to DLL/DLH (set LCR_DLAB).
        self.regs().lcr.set(self.regs().lcr.get() | (1 << 7));

        // Program baud divisor (DLL/DLH/DLF).
        self.regs().rbr.set((divider >> BST_UART_DLF_LEN) & 0xff);
        self.regs()
            .ier
            .set((divider >> (BST_UART_DLF_LEN + 8)) & 0xff);
        self.regs().dlf.set(divider & ((1 << BST_UART_DLF_LEN) - 1));

        // Clear DLAB.
        self.regs().lcr.set(self.regs().lcr.get() & !(1 << 7));

        // 8N1 frame.
        self.regs().lcr.set(self.regs().lcr.get() | 0b11);
    }

    /// DW8250 serial output
    pub fn putchar(&mut self, c: u8) {
        // Wait for last character to go (LSR_TEMT).
        while self.regs().lsr.get() & (1 << 6) == 0 {}
        self.regs().rbr.set(c as u32);
    }

    /// DW8250 serial input
    pub fn getchar(&mut self) -> Option<u8> {
        // LSR_DR: data ready.
        if self.regs().lsr.get() & 0b1 != 0 {
            Some((self.regs().rbr.get() & 0xff) as u8)
        } else {
            None
        }
    }

    /// DW8250 serial interrupt enable or disable
    pub fn set_ier(&mut self, enable: bool) {
        self.regs().ier.set(if enable { 1 } else { 0 });
    }

    /// Read the Component Parameter Register.
    pub fn cpr(&mut self) -> u32 {
        self.regs().cpr.get()
    }
}
