use ax_errno::{AxResult, ax_err};
use ax_kspin::SpinNoIrq as Mutex;
use axaddrspace::device::{AccessWidth, Port, PortRange};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};

use crate::host;

const COM1_BASE: u16 = 0x3f8;
const COM1_END: u16 = COM1_BASE + 7;

const REG_RBR_THR_DLL: u16 = 0;
const REG_IER_DLM: u16 = 1;
const REG_IIR_FCR: u16 = 2;
const REG_LCR: u16 = 3;
const REG_MCR: u16 = 4;
const REG_LSR: u16 = 5;
const REG_MSR: u16 = 6;
const REG_SCR: u16 = 7;

const IER_RX_AVAILABLE: u8 = 1 << 0;
const IER_THR_EMPTY: u8 = 1 << 1;

const IIR_NO_INTERRUPT: u8 = 0x01;
const IIR_THR_EMPTY: u8 = 0x02;
const IIR_RX_AVAILABLE: u8 = 0x04;
const IIR_FIFO_16550A: u8 = 0xc0;

const LCR_DLAB: u8 = 1 << 7;

const LSR_DATA_READY: u8 = 1 << 0;
const LSR_THR_EMPTY: u8 = 1 << 5;
const LSR_TRANSMITTER_EMPTY: u8 = 1 << 6;

const MSR_DCD: u8 = 1 << 7;
const MSR_DSR: u8 = 1 << 5;
const MSR_CTS: u8 = 1 << 4;

const FIFO_CAPACITY: usize = 128;

#[derive(Debug)]
struct SerialState {
    ier: u8,
    fcr: u8,
    lcr: u8,
    mcr: u8,
    scr: u8,
    dll: u8,
    dlm: u8,
    rx_fifo: [u8; FIFO_CAPACITY],
    rx_head: usize,
    rx_len: usize,
}

impl SerialState {
    const fn new() -> Self {
        Self {
            ier: 0,
            fcr: 0,
            lcr: 0x03,
            mcr: 0,
            scr: 0,
            dll: 1,
            dlm: 0,
            rx_fifo: [0; FIFO_CAPACITY],
            rx_head: 0,
            rx_len: 0,
        }
    }

    fn dlab(&self) -> bool {
        self.lcr & LCR_DLAB != 0
    }

    fn push_rx(&mut self, byte: u8) {
        if self.rx_len == self.rx_fifo.len() {
            return;
        }
        let tail = (self.rx_head + self.rx_len) % self.rx_fifo.len();
        self.rx_fifo[tail] = byte;
        self.rx_len += 1;
    }

    fn pop_rx(&mut self) -> Option<u8> {
        if self.rx_len == 0 {
            return None;
        }
        let byte = self.rx_fifo[self.rx_head];
        self.rx_head = (self.rx_head + 1) % self.rx_fifo.len();
        self.rx_len -= 1;
        Some(byte)
    }

    fn clear_rx(&mut self) {
        self.rx_head = 0;
        self.rx_len = 0;
    }

    fn lsr(&self) -> u8 {
        let mut value = LSR_THR_EMPTY | LSR_TRANSMITTER_EMPTY;
        if self.rx_len != 0 {
            value |= LSR_DATA_READY;
        }
        value
    }

    fn iir(&self) -> u8 {
        if self.ier & IER_RX_AVAILABLE != 0 && self.rx_len != 0 {
            IIR_FIFO_16550A | IIR_RX_AVAILABLE
        } else if self.ier & IER_THR_EMPTY != 0 {
            IIR_FIFO_16550A | IIR_THR_EMPTY
        } else {
            IIR_FIFO_16550A | IIR_NO_INTERRUPT
        }
    }
}

/// Minimal 16550-compatible COM1 UART backed by the host console.
pub struct EmulatedSerialPort {
    state: Mutex<SerialState>,
}

impl EmulatedSerialPort {
    /// Create a new COM1 UART.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(SerialState::new()),
        }
    }

    fn poll_host_input(state: &mut SerialState) {
        let mut buf = [0u8; 32];
        let read = host::read_bytes(&mut buf);
        for &byte in &buf[..read] {
            state.push_rx(byte);
        }
    }

    /// Poll host console input and return whether the UART should assert IRQ4.
    pub fn poll_irq(&self) -> bool {
        let mut state = self.state.lock();
        Self::poll_host_input(&mut state);
        state.ier & IER_RX_AVAILABLE != 0 && state.rx_len != 0
    }
}

impl Default for EmulatedSerialPort {
    fn default() -> Self {
        Self::new()
    }
}

impl BaseDeviceOps<PortRange> for EmulatedSerialPort {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Console
    }

    fn address_range(&self) -> PortRange {
        PortRange::new(Port(COM1_BASE), Port(COM1_END))
    }

    fn handle_read(&self, port: Port, width: AccessWidth) -> AxResult<usize> {
        if width != AccessWidth::Byte {
            return ax_err!(Unsupported, "x86 serial only supports byte port reads");
        }

        let mut state = self.state.lock();
        Self::poll_host_input(&mut state);
        let offset = port.0 - COM1_BASE;
        let value = match offset {
            REG_RBR_THR_DLL if state.dlab() => state.dll,
            REG_RBR_THR_DLL => state.pop_rx().unwrap_or(0),
            REG_IER_DLM if state.dlab() => state.dlm,
            REG_IER_DLM => state.ier,
            REG_IIR_FCR => state.iir(),
            REG_LCR => state.lcr,
            REG_MCR => state.mcr,
            REG_LSR => state.lsr(),
            REG_MSR => MSR_DCD | MSR_DSR | MSR_CTS,
            REG_SCR => state.scr,
            _ => return ax_err!(Unsupported, "unsupported x86 serial read port"),
        };
        Ok(value as usize)
    }

    fn handle_write(&self, port: Port, width: AccessWidth, val: usize) -> AxResult {
        if width != AccessWidth::Byte {
            return ax_err!(Unsupported, "x86 serial only supports byte port writes");
        }

        let mut state = self.state.lock();
        let offset = port.0 - COM1_BASE;
        let value = val as u8;
        match offset {
            REG_RBR_THR_DLL if state.dlab() => state.dll = value,
            REG_RBR_THR_DLL => host::write_bytes(&[value]),
            REG_IER_DLM if state.dlab() => state.dlm = value,
            REG_IER_DLM => state.ier = value & 0x0f,
            REG_IIR_FCR => {
                state.fcr = value;
                if value & (1 << 1) != 0 {
                    state.clear_rx();
                }
            }
            REG_LCR => state.lcr = value,
            REG_MCR => state.mcr = value,
            REG_LSR | REG_MSR => {}
            REG_SCR => state.scr = value,
            _ => return ax_err!(Unsupported, "unsupported x86 serial write port"),
        }
        Ok(())
    }
}
