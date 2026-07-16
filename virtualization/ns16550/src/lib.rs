//! 16550-compatible virtual UART.
//!
//! The model owns guest-visible registers, a receive FIFO, one level-triggered
//! [`IrqLine`], and a byte-oriented transmit backend. VM scheduling, host
//! console ownership, firmware generation, and interrupt-controller topology
//! remain outside this crate.

#![no_std]
#![warn(missing_docs)]

extern crate alloc;

use alloc::{format, string::String, sync::Arc};
use core::any::Any;

use ax_kspin::SpinNoIrq;
use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceError, InterruptTriggerMode,
    IrqError, IrqLine, Resource,
};

const REGISTER_COUNT: u64 = 8;
const FIFO_CAPACITY: usize = 16;

const RBR_THR_DLL: u64 = 0;
const IER_DLM: u64 = 1;
const IIR_FCR: u64 = 2;
const LCR: u64 = 3;
const MCR: u64 = 4;
const LSR: u64 = 5;
const MSR: u64 = 6;
const SCR: u64 = 7;

const IER_RX_AVAILABLE: u8 = 1 << 0;
const IER_THR_EMPTY: u8 = 1 << 1;
const IER_LINE_STATUS: u8 = 1 << 2;
const IER_MASK: u8 = 0x0f;

const IIR_NO_INTERRUPT: u8 = 0x01;
const IIR_THR_EMPTY: u8 = 0x02;
const IIR_RX_AVAILABLE: u8 = 0x04;
const IIR_LINE_STATUS: u8 = 0x06;
const IIR_FIFO_ENABLED: u8 = 0xc0;

const FCR_FIFO_ENABLE: u8 = 1 << 0;
const FCR_CLEAR_RX: u8 = 1 << 1;
const LCR_DLAB: u8 = 1 << 7;

const LSR_DATA_READY: u8 = 1 << 0;
const LSR_OVERRUN_ERROR: u8 = 1 << 1;
const LSR_THR_EMPTY: u8 = 1 << 5;
const LSR_TRANSMITTER_EMPTY: u8 = 1 << 6;
const MSR_DCD: u8 = 1 << 7;
const MSR_DSR: u8 = 1 << 5;
const MSR_CTS: u8 = 1 << 4;

/// Error returned by a host-facing transmit backend.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{operation}: {detail}")]
pub struct Ns16550BackendError {
    operation: &'static str,
    detail: String,
}

impl Ns16550BackendError {
    /// Creates a backend error with stable operation context.
    pub fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }
}

/// Byte-oriented host capability consumed by the UART transmitter.
pub trait Ns16550Backend: Send + Sync {
    /// Writes one guest-transmitted byte to the selected backend.
    fn transmit(&self, byte: u8) -> Result<(), Ns16550BackendError>;
}

/// Public virtual-UART construction and input error contract.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Ns16550Error {
    /// The supplied MMIO window is empty, too short, or overflows.
    #[error("invalid 16550 MMIO window at {base:#x} with size {size:#x}")]
    InvalidMmioWindow {
        /// Rejected guest physical base.
        base: u64,
        /// Rejected window length.
        size: u64,
    },
    /// The supplied interrupt line is not level-triggered.
    #[error("16550 requires a level-triggered IRQ line, got {actual:?}")]
    InvalidInterruptTrigger {
        /// Trigger mode of the supplied connection.
        actual: InterruptTriggerMode,
    },
    /// Updating the electrical interrupt line failed.
    #[error("16550 interrupt update failed: {0}")]
    Interrupt(#[from] IrqError),
    /// Transmitting to the host backend failed.
    #[error("16550 backend failed: {0}")]
    Backend(#[from] Ns16550BackendError),
}

/// Result returned by virtual-UART management operations.
pub type Ns16550Result<T = ()> = Result<T, Ns16550Error>;

/// Concurrent MMIO 16550 UART model with a 16-byte receive FIFO.
pub struct Ns16550 {
    name: String,
    base: u64,
    size: u64,
    resources: [Resource; 1],
    state: SpinNoIrq<UartState>,
    irq: IrqLine,
    backend: Arc<dyn Ns16550Backend>,
}

impl Ns16550 {
    /// Creates an MMIO UART attached to one level-triggered controller input.
    pub fn new_mmio(
        name: impl Into<String>,
        base: u64,
        size: u64,
        irq: IrqLine,
        backend: Arc<dyn Ns16550Backend>,
    ) -> Ns16550Result<Self> {
        if size < REGISTER_COUNT || base.checked_add(size).is_none() {
            return Err(Ns16550Error::InvalidMmioWindow { base, size });
        }
        if irq.trigger() != InterruptTriggerMode::LevelTriggered {
            return Err(Ns16550Error::InvalidInterruptTrigger {
                actual: irq.trigger(),
            });
        }
        Ok(Self {
            name: name.into(),
            base,
            size,
            resources: [Resource::MmioRange { base, size }],
            state: SpinNoIrq::new(UartState::default()),
            irq,
            backend,
        })
    }

    /// Delivers one byte from an asynchronous receive backend.
    pub fn receive(&self, byte: u8) -> Ns16550Result {
        {
            let mut state = self.state.lock();
            state.receive(byte);
            state.changed();
        }
        self.synchronize_irq()
    }

    fn checked_register(&self, access: &BusAccess) -> Result<u64, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::Unsupported {
                operation: "access 16550",
                detail: format!("expected MMIO access, got {:?}", access.kind),
            });
        }
        if access.width != AccessWidth::Byte {
            return Err(DeviceError::InvalidWidth {
                expected: AccessWidth::Byte,
                actual: access.width,
            });
        }
        let offset = access
            .addr
            .checked_sub(self.base)
            .filter(|offset| *offset < self.size)
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        Ok(offset)
    }

    fn synchronize_irq(&self) -> Ns16550Result {
        loop {
            let (desired, asserted, generation) = {
                let state = self.state.lock();
                (
                    state.interrupt_pending(),
                    state.irq_asserted,
                    state.generation,
                )
            };
            if desired == asserted {
                return Ok(());
            }
            if desired {
                self.irq.raise()?;
            } else {
                self.irq.lower()?;
            }
            let mut state = self.state.lock();
            state.irq_asserted = desired;
            if state.generation == generation && state.interrupt_pending() == desired {
                return Ok(());
            }
        }
    }
}

impl Device for Ns16550 {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let register = self.checked_register(access)?;
        if register >= REGISTER_COUNT {
            return Ok(if access.is_read {
                BusResponse::Read { value: 0 }
            } else {
                BusResponse::Write
            });
        }

        if access.is_read {
            let value = {
                let mut state = self.state.lock();
                let value = state.read(register);
                state.changed();
                value
            };
            self.synchronize_irq().map_err(device_irq_error)?;
            Ok(BusResponse::Read {
                value: u64::from(value),
            })
        } else {
            let transmitted = {
                let mut state = self.state.lock();
                let transmitted = state.write(register, access.data as u8);
                state.changed();
                transmitted
            };
            transmitted
                .map(|byte| self.backend.transmit(byte))
                .transpose()
                .map_err(device_backend_error)?;
            self.synchronize_irq().map_err(device_irq_error)?;
            Ok(BusResponse::Write)
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn reset(&mut self) -> Result<(), DeviceError> {
        *self.state.lock() = UartState::default();
        self.irq.lower().map_err(|error| DeviceError::Backend {
            operation: "reset 16550 interrupt",
            detail: format!("{error}"),
        })
    }
}

fn device_irq_error(error: Ns16550Error) -> DeviceError {
    DeviceError::Backend {
        operation: "update 16550 interrupt",
        detail: format!("{error}"),
    }
}

fn device_backend_error(error: Ns16550BackendError) -> DeviceError {
    DeviceError::Backend {
        operation: "transmit 16550 byte",
        detail: format!("{error}"),
    }
}

#[derive(Default)]
struct UartState {
    ier: u8,
    fcr: u8,
    lcr: u8,
    mcr: u8,
    scr: u8,
    dll: u8,
    dlm: u8,
    line_errors: u8,
    rx: RxFifo,
    irq_asserted: bool,
    generation: u64,
}

impl UartState {
    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn dlab(&self) -> bool {
        self.lcr & LCR_DLAB != 0
    }

    fn receive(&mut self, byte: u8) {
        if !self.rx.push(byte) {
            self.line_errors |= LSR_OVERRUN_ERROR;
        }
    }

    fn interrupt_identification(&self) -> u8 {
        let fifo = if self.fcr & FCR_FIFO_ENABLE != 0 {
            IIR_FIFO_ENABLED
        } else {
            0
        };
        if self.ier & IER_LINE_STATUS != 0 && self.line_errors != 0 {
            fifo | IIR_LINE_STATUS
        } else if self.ier & IER_RX_AVAILABLE != 0 && !self.rx.is_empty() {
            fifo | IIR_RX_AVAILABLE
        } else if self.ier & IER_THR_EMPTY != 0 {
            fifo | IIR_THR_EMPTY
        } else {
            fifo | IIR_NO_INTERRUPT
        }
    }

    fn interrupt_pending(&self) -> bool {
        self.interrupt_identification() & IIR_NO_INTERRUPT == 0
    }

    fn line_status(&self) -> u8 {
        let data_ready = if self.rx.is_empty() {
            0
        } else {
            LSR_DATA_READY
        };
        data_ready | self.line_errors | LSR_THR_EMPTY | LSR_TRANSMITTER_EMPTY
    }

    fn read(&mut self, register: u64) -> u8 {
        match register {
            RBR_THR_DLL if self.dlab() => self.dll,
            RBR_THR_DLL => self.rx.pop().unwrap_or(0),
            IER_DLM if self.dlab() => self.dlm,
            IER_DLM => self.ier,
            IIR_FCR => self.interrupt_identification(),
            LCR => self.lcr,
            MCR => self.mcr,
            LSR => {
                let value = self.line_status();
                self.line_errors = 0;
                value
            }
            MSR => MSR_DCD | MSR_DSR | MSR_CTS,
            SCR => self.scr,
            _ => 0,
        }
    }

    fn write(&mut self, register: u64, value: u8) -> Option<u8> {
        match register {
            RBR_THR_DLL if self.dlab() => self.dll = value,
            RBR_THR_DLL => return Some(value),
            IER_DLM if self.dlab() => self.dlm = value,
            IER_DLM => self.ier = value & IER_MASK,
            IIR_FCR => {
                self.fcr = value;
                if value & FCR_CLEAR_RX != 0 {
                    self.rx.clear();
                }
            }
            LCR => self.lcr = value,
            MCR => self.mcr = value,
            LSR | MSR => {}
            SCR => self.scr = value,
            _ => {}
        }
        None
    }
}

#[derive(Default)]
struct RxFifo {
    bytes: [u8; FIFO_CAPACITY],
    head: usize,
    count: usize,
}

impl RxFifo {
    fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn push(&mut self, byte: u8) -> bool {
        if self.count == self.bytes.len() {
            return false;
        }
        let tail = (self.head + self.count) % self.bytes.len();
        self.bytes[tail] = byte;
        self.count += 1;
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.is_empty() {
            return None;
        }
        let byte = self.bytes[self.head];
        self.head = (self.head + 1) % self.bytes.len();
        self.count -= 1;
        Some(byte)
    }

    fn clear(&mut self) {
        self.head = 0;
        self.count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receive_fifo_reports_data_and_overrun() {
        let mut state = UartState::default();
        state.ier = IER_RX_AVAILABLE | IER_LINE_STATUS;

        for byte in 0..FIFO_CAPACITY as u8 {
            state.receive(byte);
        }
        state.receive(0xff);

        assert_eq!(state.interrupt_identification() & 0x0f, IIR_LINE_STATUS);
        assert_ne!(state.line_status() & LSR_OVERRUN_ERROR, 0);
        assert_eq!(state.read(RBR_THR_DLL), 0);
    }

    #[test]
    fn divisor_latch_does_not_transmit() {
        let mut state = UartState::default();
        state.lcr = LCR_DLAB;

        assert_eq!(state.write(RBR_THR_DLL, 12), None);
        assert_eq!(state.dll, 12);
        assert_eq!(state.read(RBR_THR_DLL), 12);
    }

    #[test]
    fn rx_interrupt_tracks_ier_and_fifo_state() {
        let mut state = UartState::default();
        state.receive(b'a');
        assert!(!state.interrupt_pending());

        state.ier = IER_RX_AVAILABLE;
        assert!(state.interrupt_pending());

        assert_eq!(state.read(RBR_THR_DLL), b'a');
        assert!(!state.interrupt_pending());
    }
}
