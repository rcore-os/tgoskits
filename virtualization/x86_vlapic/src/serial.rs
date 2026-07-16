use alloc::sync::Arc;

use crate::{
    X86AccessWidth, X86Port, X86PortRange, X86VlapicError, X86VlapicResult,
    lock::SpinMutex as Mutex,
};

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

/// Byte-stream capability used by one emulated COM1 instance.
pub trait X86SerialBackend: Send + Sync {
    /// Consumes bytes written by the guest transmitter.
    fn transmit(&self, bytes: &[u8]);

    /// Supplies up to `bytes.len()` pending receive bytes.
    fn receive(&self, bytes: &mut [u8]) -> usize;
}

/// Minimal 16550-compatible COM1 UART with a per-instance backend.
pub struct EmulatedSerialPort {
    state: Mutex<SerialState>,
    backend: Arc<dyn X86SerialBackend>,
}

impl EmulatedSerialPort {
    /// Creates a COM1 UART using a device-instance byte-stream capability.
    pub fn new(backend: Arc<dyn X86SerialBackend>) -> Self {
        Self {
            state: Mutex::new(SerialState::new()),
            backend,
        }
    }

    fn poll_backend_input(&self) {
        let mut buf = [0u8; 32];
        let read = self.backend.receive(&mut buf).min(buf.len());
        let mut state = self.state.lock();
        for &byte in &buf[..read] {
            state.push_rx(byte);
        }
    }

    /// Poll host console input and return whether the UART should assert IRQ4.
    pub fn poll_irq(&self) -> bool {
        self.poll_backend_input();
        let state = self.state.lock();
        state.ier & IER_RX_AVAILABLE != 0 && state.rx_len != 0
    }
}

impl EmulatedSerialPort {
    /// Returns the COM1 port range.
    pub fn address_range(&self) -> X86PortRange {
        X86PortRange::new(X86Port::new(COM1_BASE), X86Port::new(COM1_END))
    }

    /// Handles a COM1 port read.
    pub fn handle_read(&self, port: X86Port, width: X86AccessWidth) -> X86VlapicResult<usize> {
        if width != X86AccessWidth::Byte {
            return Err(X86VlapicError::Unsupported);
        }

        self.poll_backend_input();
        let mut state = self.state.lock();
        let offset = port.number() - COM1_BASE;
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
            _ => return Err(X86VlapicError::Unsupported),
        };
        Ok(value as usize)
    }

    /// Handles a COM1 port write.
    pub fn handle_write(
        &self,
        port: X86Port,
        width: X86AccessWidth,
        val: usize,
    ) -> X86VlapicResult {
        if width != X86AccessWidth::Byte {
            return Err(X86VlapicError::Unsupported);
        }

        let value = val as u8;
        let transmit = {
            let mut state = self.state.lock();
            let offset = port.number() - COM1_BASE;
            match offset {
                REG_RBR_THR_DLL if state.dlab() => {
                    state.dll = value;
                    None
                }
                REG_RBR_THR_DLL => Some(value),
                REG_IER_DLM if state.dlab() => {
                    state.dlm = value;
                    None
                }
                REG_IER_DLM => {
                    state.ier = value & 0x0f;
                    None
                }
                REG_IIR_FCR => {
                    state.fcr = value;
                    if value & (1 << 1) != 0 {
                        state.clear_rx();
                    }
                    None
                }
                REG_LCR => {
                    state.lcr = value;
                    None
                }
                REG_MCR => {
                    state.mcr = value;
                    None
                }
                REG_LSR | REG_MSR => None,
                REG_SCR => {
                    state.scr = value;
                    None
                }
                _ => return Err(X86VlapicError::Unsupported),
            }
        };
        if let Some(byte) = transmit {
            self.backend.transmit(&[byte]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};

    use super::*;
    #[test]
    fn per_instance_backend_replaces_static_host_console_callbacks() {
        let backend = Arc::new(TestSerialBackend::new(b"input"));
        let serial = EmulatedSerialPort::new(backend.clone());

        serial
            .handle_write(X86Port::new(COM1_BASE), X86AccessWidth::Byte, b'X' as usize)
            .unwrap();
        assert_eq!(backend.transmitted(), b"X");
        assert_eq!(
            serial
                .handle_read(X86Port::new(COM1_BASE), X86AccessWidth::Byte)
                .unwrap(),
            b'i' as usize
        );
    }

    struct TestSerialBackend {
        rx: Mutex<Vec<u8>>,
        tx: Mutex<Vec<u8>>,
    }

    impl TestSerialBackend {
        fn new(rx: &[u8]) -> Self {
            Self {
                rx: Mutex::new(rx.into()),
                tx: Mutex::new(Vec::new()),
            }
        }

        fn transmitted(&self) -> Vec<u8> {
            self.tx.lock().clone()
        }
    }

    impl X86SerialBackend for TestSerialBackend {
        fn transmit(&self, bytes: &[u8]) {
            self.tx.lock().extend_from_slice(bytes);
        }

        fn receive(&self, bytes: &mut [u8]) -> usize {
            let mut pending = self.rx.lock();
            let count = pending.len().min(bytes.len());
            bytes[..count].copy_from_slice(&pending[..count]);
            pending.drain(..count);
            count
        }
    }
}
