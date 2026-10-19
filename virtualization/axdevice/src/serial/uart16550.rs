//! Reusable 16550-compatible UART register core.

use alloc::sync::Arc;

use ax_sync::{Mutex, MutexGuard};
use axdevice_base::{AccessWidth, DeviceError, DeviceResult, IrqLine};

use super::{
    SerialBackend, SerialEndpoint,
    endpoint::RX_POLL_BUFFER_SIZE,
    fifo::{ByteFifo, RxFifo},
};

const REG_RBR_THR_DLL: usize = 0;
const REG_IER_DLM: usize = 1;
const REG_IIR_FCR: usize = 2;
const REG_LCR: usize = 3;
const REG_MCR: usize = 4;
const REG_LSR: usize = 5;
const REG_MSR: usize = 6;
const REG_SCR: usize = 7;

const IER_RX_AVAILABLE: u8 = 1 << 0;
const IER_THR_EMPTY: u8 = 1 << 1;

const IIR_NO_INTERRUPT: u8 = 0x01;
const IIR_THR_EMPTY: u8 = 0x02;
const IIR_RX_AVAILABLE: u8 = 0x04;
const IIR_FIFO_16550A: u8 = 0xc0;

const FCR_ENABLE_FIFO: u8 = 1 << 0;
const FCR_CLEAR_RX: u8 = 1 << 1;
const FCR_CLEAR_TX: u8 = 1 << 2;
const LCR_DLAB: u8 = 1 << 7;
const MCR_LOOPBACK: u8 = 1 << 4;

const LSR_DATA_READY: u8 = 1 << 0;
const LSR_OVERRUN_ERROR: u8 = 1 << 1;
const LSR_THR_EMPTY: u8 = 1 << 5;
const LSR_TRANSMITTER_EMPTY: u8 = 1 << 6;

const MSR_DCD: u8 = 1 << 7;
const MSR_DSR: u8 = 1 << 5;
const MSR_CTS: u8 = 1 << 4;

const FIFO_CAPACITY: usize = 256;
const TX_FIFO_CAPACITY: usize = 16;

struct Uart16550State {
    ier: u8,
    fcr: u8,
    lcr: u8,
    mcr: u8,
    scr: u8,
    dll: u8,
    dlm: u8,
    overrun: bool,
    tx_interrupt_pending: bool,
    tx_fifo: ByteFifo<TX_FIFO_CAPACITY>,
    tx_drain_active: bool,
    tx_drain_requested: bool,
    tx_reset_during_drain: bool,
    rx_fifo: RxFifo<FIFO_CAPACITY>,
}

impl Uart16550State {
    const fn new() -> Self {
        Self {
            ier: 0,
            fcr: 0,
            lcr: 0x03,
            mcr: 0,
            scr: 0,
            dll: 1,
            dlm: 0,
            overrun: false,
            tx_interrupt_pending: true,
            tx_fifo: ByteFifo::new(),
            tx_drain_active: false,
            tx_drain_requested: false,
            tx_reset_during_drain: false,
            rx_fifo: RxFifo::new(),
        }
    }

    const fn dlab(&self) -> bool {
        self.lcr & LCR_DLAB != 0
    }

    fn push_rx(&mut self, byte: u8) {
        if !self.rx_fifo.push(byte) {
            self.overrun = true;
        }
    }

    fn line_status(&mut self) -> u8 {
        let mut status = 0;
        if self.tx_empty() {
            status |= LSR_THR_EMPTY | LSR_TRANSMITTER_EMPTY;
        }
        if !self.rx_fifo.is_empty() {
            status |= LSR_DATA_READY;
        }
        if core::mem::take(&mut self.overrun) {
            status |= LSR_OVERRUN_ERROR;
        }
        status
    }

    const fn tx_empty(&self) -> bool {
        self.tx_fifo.is_empty() && !self.tx_drain_active
    }

    const fn fifo_interrupt_bits(&self) -> u8 {
        if self.fcr & FCR_ENABLE_FIFO != 0 {
            IIR_FIFO_16550A
        } else {
            0
        }
    }

    const fn pending_interrupt(&self) -> u8 {
        if self.ier & IER_RX_AVAILABLE != 0 && !self.rx_fifo.is_empty() {
            IIR_RX_AVAILABLE
        } else if self.ier & IER_THR_EMPTY != 0 && self.tx_interrupt_pending && self.tx_empty() {
            IIR_THR_EMPTY
        } else {
            IIR_NO_INTERRUPT
        }
    }

    const fn irq_asserted(&self) -> bool {
        self.pending_interrupt() != IIR_NO_INTERRUPT
    }
}

/// 16550-compatible UART core with an external byte backend and virtual IRQ.
pub struct Uart16550 {
    state: Mutex<Uart16550State>,
    endpoint: SerialEndpoint,
}

impl Uart16550 {
    /// Creates a powered-on 16550 UART.
    pub fn new(backend: Arc<dyn SerialBackend>, irq: IrqLine) -> Self {
        Self {
            state: Mutex::new(Uart16550State::new()),
            endpoint: SerialEndpoint::new(backend, irq, "signal 16550 IRQ"),
        }
    }

    /// Locks the register/FIFO state.
    ///
    /// The state is task-sleepable: a vCPU's MMIO/poll entry runs in ordinary
    /// task context, and every backend/IRQ callback is invoked only after this
    /// guard is dropped.
    fn state(&self) -> MutexGuard<'_, Uart16550State> {
        self.state.lock()
    }

    fn drain_tx(&self) -> DeviceResult {
        {
            let mut state = self.state();
            if state.tx_drain_active {
                // A poll may carry the only capacity-release notification.
                // Its request belongs to the active drainer until completion.
                state.tx_drain_requested = true;
                return Ok(());
            }
            if state.tx_fifo.is_empty() {
                return Ok(());
            }
            state.tx_drain_active = true;
        }

        loop {
            let mut byte = [0];
            {
                let mut state = self.state();
                if state.tx_fifo.copy_to(&mut byte) == 0 {
                    state.tx_drain_active = false;
                    state.tx_drain_requested = false;
                    state.tx_interrupt_pending = true;
                    drop(state);
                    return self.refresh_irq();
                }
                state.tx_reset_during_drain = false;
            }

            // Only one byte is in flight, like a shift register. FCR may clear
            // queued bytes during this callback, but cannot retract this byte.
            let accepted = self.endpoint.try_write(&byte);
            let mut state = self.state();
            let reset = core::mem::take(&mut state.tx_reset_during_drain);
            if !reset {
                state.tx_fifo.discard(accepted);
            }
            let requested = core::mem::take(&mut state.tx_drain_requested);
            if accepted == 0 && !reset && !requested {
                state.tx_drain_active = false;
                drop(state);
                return self.refresh_irq();
            }
        }
    }

    /// Polls backend input into the receive FIFO and refreshes the level IRQ.
    ///
    /// Concurrent polls wait for the active backend read. A backend may access
    /// registers, but must not recursively poll the UART from its read callback.
    pub fn poll(&self) -> DeviceResult {
        self.drain_tx()?;
        self.fill_rx(self.endpoint.lock_rx())
    }

    fn fill_rx(&self, _read: MutexGuard<'_, ()>) -> DeviceResult {
        let capacity = self.state().rx_fifo.begin_read(RX_POLL_BUFFER_SIZE);
        let Some(capacity) = capacity else {
            return self.refresh_irq();
        };
        self.endpoint.poll_rx(
            capacity,
            |bytes| self.state().rx_fifo.finish_read(bytes),
            || self.state().irq_asserted(),
        )
    }

    fn refresh_irq(&self) -> DeviceResult {
        self.endpoint.refresh_irq(|| self.state().irq_asserted())
    }

    /// Reads one UART register.
    pub fn read(&self, register: usize, width: AccessWidth) -> DeviceResult<u64> {
        if width != AccessWidth::Byte {
            return Err(DeviceError::InvalidWidth {
                expected: AccessWidth::Byte,
                actual: width,
            });
        }

        let (value, refill) = {
            let mut state = self.state();
            let receive = register == REG_RBR_THR_DLL && !state.dlab();
            let value = match register {
                REG_RBR_THR_DLL if state.dlab() => state.dll,
                REG_RBR_THR_DLL => state.rx_fifo.pop().unwrap_or(0),
                REG_IER_DLM if state.dlab() => state.dlm,
                REG_IER_DLM => state.ier,
                REG_IIR_FCR => {
                    let interrupt = state.pending_interrupt();
                    if interrupt == IIR_THR_EMPTY {
                        state.tx_interrupt_pending = false;
                    }
                    state.fifo_interrupt_bits() | interrupt
                }
                REG_LCR => state.lcr,
                REG_MCR => state.mcr,
                REG_LSR => state.line_status(),
                REG_MSR => MSR_DCD | MSR_DSR | MSR_CTS,
                REG_SCR => state.scr,
                _ => 0,
            };
            (value, receive && state.rx_fifo.is_empty())
        };
        if refill && let Some(read) = self.endpoint.try_lock_rx() {
            // Consuming the last byte makes room for the next bounded batch.
            // Progress must not depend on another coalesced device-work wake.
            self.fill_rx(read)?;
        } else {
            self.refresh_irq()?;
        }
        Ok(value as u64)
    }

    /// Writes one UART register.
    pub fn write(&self, register: usize, width: AccessWidth, value: u64) -> DeviceResult {
        if width != AccessWidth::Byte {
            return Err(DeviceError::InvalidWidth {
                expected: AccessWidth::Byte,
                actual: width,
            });
        }

        let byte = value as u8;
        let mut drain_tx = false;
        let mut completes_loopback = false;
        // Publish the consumed level before a concurrent drainer can publish
        // completion, so an edge-triggered controller can rearm THRE.
        let consumed_result = self.endpoint.refresh_irq(|| {
            let mut state = self.state();
            match register {
                REG_RBR_THR_DLL if state.dlab() => state.dll = byte,
                REG_RBR_THR_DLL => {
                    state.tx_interrupt_pending = false;
                    if state.mcr & MCR_LOOPBACK != 0 {
                        state.push_rx(byte);
                        completes_loopback = true;
                    } else {
                        // This model retains at most 16 queued TX bytes and,
                        // with FIFOs disabled, admits a byte only when empty.
                        // Excess writes discard the newest byte, preserving
                        // queued output. Guests must honor THRE and FIFO depth;
                        // host backpressure does not create unbounded storage.
                        if state.fcr & FCR_ENABLE_FIFO != 0 || state.tx_fifo.is_empty() {
                            state.tx_fifo.push(byte);
                        }
                        drain_tx = true;
                    }
                }
                REG_IER_DLM if state.dlab() => state.dlm = byte,
                REG_IER_DLM => {
                    let old = state.ier;
                    state.ier = byte & 0x0f;
                    if (old ^ state.ier) & IER_THR_EMPTY != 0 {
                        state.tx_interrupt_pending =
                            state.ier & IER_THR_EMPTY != 0 && state.tx_empty();
                    }
                }
                REG_IIR_FCR => {
                    state.fcr = byte;
                    if byte & FCR_CLEAR_RX != 0 {
                        state.rx_fifo.clear();
                        state.overrun = false;
                    }
                    if byte & FCR_CLEAR_TX != 0 {
                        state.tx_fifo.clear();
                        state.tx_reset_during_drain = state.tx_drain_active;
                        state.tx_interrupt_pending = true;
                    }
                }
                REG_LCR => state.lcr = byte,
                REG_MCR => state.mcr = byte,
                REG_LSR | REG_MSR => {}
                REG_SCR => state.scr = byte,
                _ => {}
            }
            state.irq_asserted()
        });
        if drain_tx {
            return consumed_result.and(self.drain_tx());
        }
        if !completes_loopback {
            if register == REG_IIR_FCR
                && byte & FCR_CLEAR_RX != 0
                && let Some(read) = self.endpoint.try_lock_rx()
            {
                // Reset also releases RX capacity. Bytes still in the backend
                // are subsequent arrivals, not part of the discarded FIFO.
                return consumed_result.and(self.fill_rx(read));
            }
            return consumed_result;
        }

        // Loopback completes when the byte reaches the receive FIFO. The
        // explicit low/high sequence is required even though no guest instruction runs
        // between the two phases: an edge-configured IOAPIC must observe THRE being
        // consumed before it can deliver the next empty interrupt. Deferring this phase
        // to DeviceRuntime::poll would make TX progress depend on unrelated RX polling.
        self.state().tx_interrupt_pending = true;
        let completed_result = self.refresh_irq();
        consumed_result.and(completed_result)
    }
}
