//! Arm PrimeCell PL011 virtual UART.
//!
//! The model owns only UART state, one level-triggered [`IrqLine`], and a
//! byte-oriented transmit capability. Host console ownership, scheduling, and
//! vCPU interaction remain outside this crate.

#![no_std]
#![warn(missing_docs)]

extern crate alloc;

mod registers;
mod state;

use alloc::{format, string::String, sync::Arc};
use core::any::Any;

use ax_kspin::SpinNoIrq;
use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceError, InterruptTriggerMode,
    IrqError, IrqLine, Resource,
};
use registers::{MMIO_SIZE, UARTDR};
use state::Pl011State;

bitflags::bitflags! {
    /// Error status associated with one received character.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct RxErrors: u8 {
        /// Framing error.
        const FRAMING = 1 << 0;
        /// Parity error.
        const PARITY = 1 << 1;
        /// Break condition.
        const BREAK = 1 << 2;
        /// Receive overrun.
        const OVERRUN = 1 << 3;
    }
}

/// Outcome of delivering one host byte to the virtual UART.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RxResult {
    /// The byte entered the receive FIFO.
    Accepted,
    /// The FIFO was full; the byte was discarded and overrun was latched.
    DroppedOverrun,
    /// The guest has not enabled the receiver.
    ReceiverDisabled,
}

/// Error returned by a host-facing transmit backend.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{operation}: {detail}")]
pub struct Pl011BackendError {
    operation: &'static str,
    detail: String,
}

impl Pl011BackendError {
    /// Creates a backend error with stable operation context.
    pub fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }
}

/// Byte-oriented host capability consumed by the PL011 transmitter.
pub trait Pl011Backend: Send + Sync {
    /// Writes one guest-transmitted byte to the selected backend.
    fn transmit(&self, byte: u8) -> Result<(), Pl011BackendError>;
}

/// Public PL011 construction and asynchronous-input error contract.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Pl011Error {
    /// The MMIO window was not aligned or overflowed.
    #[error("invalid PL011 MMIO base {base:#x}")]
    InvalidMmioBase {
        /// Rejected guest physical base.
        base: u64,
    },
    /// The supplied interrupt connection was not level-triggered.
    #[error("PL011 requires a level-triggered IRQ line, got {actual:?}")]
    InvalidInterruptTrigger {
        /// Trigger mode of the supplied line.
        actual: InterruptTriggerMode,
    },
    /// Updating the electrical interrupt line failed.
    #[error("PL011 interrupt update failed: {0}")]
    Interrupt(#[from] IrqError),
    /// Transmitting to the host backend failed.
    #[error("PL011 backend failed: {0}")]
    Backend(#[from] Pl011BackendError),
}

/// Result returned by PL011 management-path operations.
pub type Pl011Result<T = ()> = Result<T, Pl011Error>;

/// Concurrent PL011 UART model with a 16-byte receive FIFO.
pub struct Pl011 {
    name: String,
    base: u64,
    resources: [Resource; 1],
    state: SpinNoIrq<Pl011State>,
    irq: IrqLine,
    backend: Arc<dyn Pl011Backend>,
}

impl Pl011 {
    /// Creates a PL011 attached to one level-triggered controller input.
    pub fn new(
        name: impl Into<String>,
        base: u64,
        irq: IrqLine,
        backend: Arc<dyn Pl011Backend>,
    ) -> Pl011Result<Self> {
        if !base.is_multiple_of(MMIO_SIZE) || base.checked_add(MMIO_SIZE).is_none() {
            return Err(Pl011Error::InvalidMmioBase { base });
        }
        if irq.trigger() != InterruptTriggerMode::LevelTriggered {
            return Err(Pl011Error::InvalidInterruptTrigger {
                actual: irq.trigger(),
            });
        }
        Ok(Self {
            name: name.into(),
            base,
            resources: [Resource::MmioRange {
                base,
                size: MMIO_SIZE,
            }],
            state: SpinNoIrq::new(Pl011State::default()),
            irq,
            backend,
        })
    }

    /// Delivers one character from the selected host receive backend.
    pub fn receive(&self, byte: u8, errors: RxErrors) -> Pl011Result<RxResult> {
        let result = {
            let mut state = self.state.lock();
            let result = state.receive(byte, errors);
            state.changed();
            result
        };
        self.synchronize_irq()?;
        Ok(result)
    }

    /// Reports whether the receiver can currently accept one buffered backend byte.
    ///
    /// Host adapters use this as backpressure before removing another byte from
    /// their own queue. A concurrent guest register access may still change the
    /// result before [`Self::receive`] runs, so callers must also handle its
    /// returned [`RxResult`].
    pub fn receive_ready(&self) -> bool {
        self.state.lock().receive_ready()
    }

    /// Expires the architectural receive-timeout interval.
    ///
    /// The AxVM timer adapter calls this after 32 bit periods without a new
    /// character while unread data remains in the FIFO.
    pub fn expire_receive_timeout(&self) -> Pl011Result {
        self.state.lock().expire_receive_timeout();
        self.synchronize_irq()
    }

    fn read_register(&self, offset: u64) -> u32 {
        let mut state = self.state.lock();
        let value = state.read(offset);
        if matches!(offset, UARTDR) {
            state.changed();
        }
        value
    }

    fn write_register(&self, offset: u64, value: u32) -> Option<u8> {
        let mut state = self.state.lock();
        let transmitted = state.write(offset, value);
        state.changed();
        transmitted
    }

    fn synchronize_irq(&self) -> Pl011Result {
        loop {
            let (desired, asserted, generation) = self.state.lock().irq_snapshot();
            if desired == asserted {
                return Ok(());
            }

            if desired {
                self.irq.raise()?;
            } else {
                self.irq.lower()?;
            }

            if self.state.lock().record_irq_level(desired, generation) {
                return Ok(());
            }
        }
    }

    fn checked_offset(&self, access: &BusAccess) -> Result<u64, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::Unsupported {
                operation: "access PL011",
                detail: format!("expected MMIO access, got {:?}", access.kind),
            });
        }
        if !matches!(
            access.width,
            AccessWidth::Byte | AccessWidth::Word | AccessWidth::Dword
        ) {
            return Err(DeviceError::InvalidWidth {
                expected: AccessWidth::Dword,
                actual: access.width,
            });
        }
        let offset = access
            .addr
            .checked_sub(self.base)
            .filter(|offset| *offset < MMIO_SIZE)
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        if !offset.is_multiple_of(4) {
            return Err(DeviceError::InvalidInput {
                operation: "access PL011 register",
                detail: format!("register offset {offset:#x} is not 32-bit aligned"),
            });
        }
        Ok(offset)
    }

    const fn access_mask(width: AccessWidth) -> u32 {
        match width {
            AccessWidth::Byte => u8::MAX as u32,
            AccessWidth::Word => u16::MAX as u32,
            AccessWidth::Dword | AccessWidth::Qword => u32::MAX,
        }
    }
}

impl Device for Pl011 {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let offset = self.checked_offset(access)?;
        let mask = Self::access_mask(access.width);
        if access.is_read {
            let value = self.read_register(offset) & mask;
            self.synchronize_irq().map_err(device_irq_error)?;
            Ok(BusResponse::Read {
                value: u64::from(value),
            })
        } else {
            let transmitted = self.write_register(offset, access.data as u32 & mask);
            let backend_result = transmitted
                .map(|byte| self.backend.transmit(byte))
                .transpose();
            let interrupt_result = self.synchronize_irq();
            backend_result.map_err(device_backend_error)?;
            interrupt_result.map_err(device_irq_error)?;
            Ok(BusResponse::Write)
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn reset(&mut self) -> Result<(), DeviceError> {
        *self.state.lock() = Pl011State::default();
        self.irq.lower().map_err(|error| DeviceError::Backend {
            operation: "reset PL011 interrupt",
            detail: format!("{error}"),
        })
    }
}

fn device_irq_error(error: Pl011Error) -> DeviceError {
    DeviceError::Backend {
        operation: "update PL011 interrupt",
        detail: format!("{error}"),
    }
}

fn device_backend_error(error: Pl011BackendError) -> DeviceError {
    DeviceError::Backend {
        operation: "transmit PL011 byte",
        detail: format!("{error}"),
    }
}
