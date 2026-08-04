//! Arm PrimeCell PL011 UART register model.

use alloc::sync::Arc;

use ax_kspin::SpinRaw;
use axdevice_base::{AccessWidth, DeviceError, DeviceResult, IrqLine};

use super::{SerialBackend, SerialEndpoint, fifo::ByteFifo};

const REG_DR: usize = 0x000;
const REG_RSR_ECR: usize = 0x004;
const REG_FR: usize = 0x018;
const REG_IBRD: usize = 0x024;
const REG_FBRD: usize = 0x028;
const REG_LCRH: usize = 0x02c;
const REG_CR: usize = 0x030;
const REG_IFLS: usize = 0x034;
const REG_IMSC: usize = 0x038;
const REG_RIS: usize = 0x03c;
const REG_MIS: usize = 0x040;
const REG_ICR: usize = 0x044;
const REG_DMACR: usize = 0x048;
const REGISTER_BLOCK_SIZE: usize = 0x1000;

const FR_TXFE: u32 = 1 << 7;
const FR_RXFF: u32 = 1 << 6;
const FR_RXFE: u32 = 1 << 4;
const FR_CTS: u32 = 1;

const INT_RX: u32 = 1 << 4;
const INT_TX: u32 = 1 << 5;
const INT_RX_TIMEOUT: u32 = 1 << 6;

const CR_UART_ENABLE: u32 = 1;
const CR_TX_ENABLE: u32 = 1 << 8;
const CR_RX_ENABLE: u32 = 1 << 9;

const FIFO_CAPACITY: usize = 256;

struct Pl011State {
    rx_fifo: ByteFifo<FIFO_CAPACITY>,
    integer_baud_rate: u32,
    fractional_baud_rate: u32,
    line_control: u32,
    control: u32,
    interrupt_fifo_level: u32,
    interrupt_mask: u32,
    dma_control: u32,
    tx_interrupt_pending: bool,
    receive_error: u32,
}

impl Pl011State {
    const fn new() -> Self {
        Self {
            rx_fifo: ByteFifo::new(),
            integer_baud_rate: 1,
            fractional_baud_rate: 0,
            line_control: 0,
            control: CR_UART_ENABLE | CR_TX_ENABLE | CR_RX_ENABLE,
            interrupt_fifo_level: 0x12,
            interrupt_mask: 0,
            dma_control: 0,
            tx_interrupt_pending: true,
            receive_error: 0,
        }
    }

    fn push_rx(&mut self, byte: u8) {
        if !self.rx_fifo.push(byte) {
            self.receive_error |= 1 << 3;
        }
    }

    fn flags(&self) -> u32 {
        let mut flags = FR_TXFE | FR_CTS;
        if self.rx_fifo.is_empty() {
            flags |= FR_RXFE;
        }
        if self.rx_fifo.is_full() {
            flags |= FR_RXFF;
        }
        flags
    }

    fn raw_interrupts(&self) -> u32 {
        let mut interrupts = 0;
        if !self.rx_fifo.is_empty() && self.control & CR_RX_ENABLE != 0 {
            interrupts |= INT_RX | INT_RX_TIMEOUT;
        }
        if self.tx_interrupt_pending && self.control & CR_TX_ENABLE != 0 {
            interrupts |= INT_TX;
        }
        interrupts
    }

    fn irq_asserted(&self) -> bool {
        self.raw_interrupts() & self.interrupt_mask != 0
    }
}

/// PL011 UART core with an external byte backend and virtual IRQ.
pub struct Pl011 {
    state: SpinRaw<Pl011State>,
    endpoint: SerialEndpoint,
}

impl Pl011 {
    /// Creates a powered-on PL011 UART.
    pub fn new(backend: Arc<dyn SerialBackend>, irq: IrqLine) -> Self {
        Self {
            state: SpinRaw::new(Pl011State::new()),
            endpoint: SerialEndpoint::new(backend, irq, "signal PL011 IRQ"),
        }
    }

    /// Polls backend input into the receive FIFO and refreshes the level IRQ.
    pub fn poll(&self) -> DeviceResult {
        self.endpoint.poll_rx(|bytes| {
            let mut state = self.state.lock();
            for &byte in bytes {
                state.push_rx(byte);
            }
            state.irq_asserted()
        })
    }

    /// Reads one PL011 register.
    pub fn read(&self, offset: usize, width: AccessWidth) -> DeviceResult<u64> {
        validate_width(width)?;
        let (value, asserted) = {
            let mut state = self.state.lock();
            let value = match offset {
                REG_DR => state.rx_fifo.pop().unwrap_or(0) as u32 | state.receive_error,
                REG_RSR_ECR => state.receive_error,
                REG_FR => state.flags(),
                REG_IBRD => state.integer_baud_rate,
                REG_FBRD => state.fractional_baud_rate,
                REG_LCRH => state.line_control,
                REG_CR => state.control,
                REG_IFLS => state.interrupt_fifo_level,
                REG_IMSC => state.interrupt_mask,
                REG_RIS => state.raw_interrupts(),
                REG_MIS => state.raw_interrupts() & state.interrupt_mask,
                REG_DMACR => state.dma_control,
                0xfe0..=0xffc if offset & 3 == 0 => peripheral_id(offset),
                _ if offset < REGISTER_BLOCK_SIZE => 0,
                _ => {
                    return Err(DeviceError::OutOfRange {
                        addr: offset as u64,
                    });
                }
            };
            (value, state.irq_asserted())
        };
        self.endpoint.set_irq_level(asserted)?;
        Ok(value as u64)
    }

    /// Writes one PL011 register.
    pub fn write(&self, offset: usize, width: AccessWidth, value: u64) -> DeviceResult {
        validate_width(width)?;
        let value = value as u32;
        let (output, asserted) = {
            let mut state = self.state.lock();
            let mut output = None;
            match offset {
                REG_DR => {
                    if state.control & (CR_UART_ENABLE | CR_TX_ENABLE)
                        == (CR_UART_ENABLE | CR_TX_ENABLE)
                    {
                        output = Some(value as u8);
                        state.tx_interrupt_pending = true;
                    }
                }
                REG_RSR_ECR => state.receive_error = 0,
                REG_IBRD => state.integer_baud_rate = value & 0xffff,
                REG_FBRD => state.fractional_baud_rate = value & 0x3f,
                REG_LCRH => state.line_control = value,
                REG_CR => state.control = value & 0xffff,
                REG_IFLS => state.interrupt_fifo_level = value & 0x3f,
                REG_IMSC => state.interrupt_mask = value & 0x7ff,
                REG_ICR => {
                    if value & INT_TX != 0 {
                        state.tx_interrupt_pending = false;
                    }
                    if value & (INT_RX | INT_RX_TIMEOUT) != 0 && state.rx_fifo.is_empty() {
                        state.receive_error = 0;
                    }
                }
                REG_DMACR => state.dma_control = value & 0x7,
                REG_FR | REG_RIS | REG_MIS | 0xfe0..=0xffc => {}
                _ if offset < REGISTER_BLOCK_SIZE => {}
                _ => {
                    return Err(DeviceError::OutOfRange {
                        addr: offset as u64,
                    });
                }
            }
            (output, state.irq_asserted())
        };

        if let Some(byte) = output {
            self.endpoint.write(core::slice::from_ref(&byte));
        }
        self.endpoint.set_irq_level(asserted)
    }
}

fn validate_width(width: AccessWidth) -> DeviceResult {
    if matches!(
        width,
        AccessWidth::Byte | AccessWidth::Word | AccessWidth::Dword
    ) {
        Ok(())
    } else {
        Err(DeviceError::InvalidWidth {
            expected: AccessWidth::Dword,
            actual: width,
        })
    }
}

fn peripheral_id(offset: usize) -> u32 {
    const IDS: [u32; 8] = [0x11, 0x10, 0x14, 0x00, 0x0d, 0xf0, 0x05, 0xb1];
    IDS[(offset - 0xfe0) / 4]
}
