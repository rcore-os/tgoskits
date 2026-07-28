use core::{
    marker::PhantomData,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{Config, ConfigError, RxSample, SerialEventSet, SerialIrqEvent};

/// Non-blocking exclusion for aliases of one UART register block.
///
/// Normal task and IRQ endpoints use same-CPU IRQ exclusion. This additional
/// gate serializes the cross-CPU emergency endpoint without allowing hard IRQ
/// or panic paths to wait.
pub struct UartRegisterGate<E: ?Sized = dyn UartEmergencyTx> {
    active: AtomicBool,
    emergency_tx: E,
}

impl<E> UartRegisterGate<E> {
    pub const fn new(emergency_tx: E) -> Self {
        Self {
            active: AtomicBool::new(false),
            emergency_tx,
        }
    }
}

impl<E: ?Sized> UartRegisterGate<E> {
    pub fn try_enter(&self) -> Option<UartRegisterGuard<'_, E>> {
        self.active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| UartRegisterGuard {
                gate: self,
                _not_send: PhantomData,
            })
    }
}

/// Move-only proof that the caller exclusively owns UART register access.
pub struct UartRegisterGuard<'a, E: ?Sized = dyn UartEmergencyTx> {
    gate: &'a UartRegisterGate<E>,
    _not_send: PhantomData<*mut ()>,
}

impl<E: UartEmergencyTx + ?Sized> UartRegisterGuard<'_, E> {
    /// Performs one bounded emergency write through this guard's UART.
    pub fn try_write(&self, bytes: &[u8]) -> usize {
        // SAFETY: this guard is created only by `self.gate`, remains borrowed
        // for the call, and releases the gate on drop.
        unsafe { self.gate.emergency_tx.try_write_unlocked(bytes) }
    }
}

impl<E: ?Sized> Drop for UartRegisterGuard<'_, E> {
    fn drop(&mut self) {
        self.gate.active.store(false, Ordering::Release);
    }
}

/// Immutable information reported by a concrete UART before it is split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UartInfo {
    pub name: &'static str,
    pub register_base: usize,
    pub initial_baudrate: u32,
}

/// Independently owned task, hard-IRQ, and emergency-TX endpoints.
pub struct UartParts<P, I, E> {
    /// Task-context data and control endpoint.
    pub port: P,
    /// Hard-IRQ event and bounded-RX endpoint.
    pub irq: I,
    /// Panic-only non-blocking transmitter endpoint.
    pub emergency_tx: E,
}

impl<P, I, E> UartParts<P, I, E> {
    pub const fn new(port: P, irq: I, emergency_tx: E) -> Self {
        Self {
            port,
            irq,
            emergency_tx,
        }
    }
}

/// Converts a concrete UART into disjoint runtime endpoints.
pub trait SplitUart: Sized {
    type Port: UartPort;
    type Irq: UartIrq;
    type EmergencyTx: UartEmergencyTx;

    fn runtime_info(&self) -> UartInfo;

    fn split(self) -> UartParts<Self::Port, Self::Irq, Self::EmergencyTx>;
}

/// UART data/control endpoint owned by one runtime maintenance task.
///
/// All calls must run on the same CPU as the associated [`UartIrq`] with local
/// device IRQ delivery excluded. This is a device-serialization contract, not
/// a memory-safety precondition.
pub trait UartPort: Send + 'static {
    /// Initializes the UART while leaving every device interrupt source masked.
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError>;

    fn shutdown(&mut self);

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError>;

    /// Reads one normalized hardware sample without consulting IRQ state.
    fn read_rx(&mut self) -> Option<RxSample>;

    /// Writes as much of `bytes` as the hardware can currently accept.
    fn write_tx(&mut self, bytes: &[u8]) -> usize;

    /// Returns whether both the FIFO and transmitter shift register are empty.
    fn tx_idle(&mut self) -> bool;

    fn mask_all(&mut self);

    /// Rearms `sources` and closes the enable/readiness race.
    ///
    /// Sources that are already ready after being enabled are masked again and
    /// returned so the maintenance task can immediately continue servicing
    /// them instead of relying on a possibly lost edge.
    fn rearm(&mut self, sources: SerialEventSet) -> SerialEventSet;
}

/// Non-blocking destination for samples drained by a UART hard-IRQ endpoint.
///
/// Implementations must be allocation-free and IRQ-safe. `push` deliberately
/// has no backpressure result: a hard IRQ cannot wait for capacity, so the
/// runtime sink owns overflow accounting and sticky error publication.
pub trait IrqRxSink {
    fn push(&mut self, sample: RxSample);
}

/// UART hard-IRQ endpoint owned by the registered IRQ callback.
pub trait UartIrq: Send + 'static {
    /// Handles the current hardware event and drains a bounded RX batch.
    ///
    /// `None` means the shared interrupt was not raised by this UART. The
    /// implementation may read RX FIFO data only through `rx`; it must never
    /// write TX FIFO data.
    fn handle(&mut self, rx: &mut dyn IrqRxSink) -> Option<SerialIrqEvent>;
}

/// Raw panic/emergency-only TX endpoint.
///
/// This endpoint deliberately exposes no configuration, interrupt, RX, or
/// blocking operation. A runtime must move it into [`UartRegisterGate`] before
/// exposing safe emergency output.
pub trait UartEmergencyTx: Send + Sync + 'static {
    /// Performs one bounded pass over currently available FIFO capacity.
    ///
    /// # Safety
    ///
    /// The caller must exclusively own every alias of this UART register
    /// block for the duration of the call. Normal users must call
    /// [`UartRegisterGuard::try_write`] instead.
    unsafe fn try_write_unlocked(&self, bytes: &[u8]) -> usize;
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicUsize;

    use super::*;

    struct RecordingEmergencyTx(&'static AtomicUsize);

    impl UartEmergencyTx for RecordingEmergencyTx {
        unsafe fn try_write_unlocked(&self, bytes: &[u8]) -> usize {
            self.0.fetch_add(bytes.len(), Ordering::Relaxed);
            bytes.len()
        }
    }

    #[test]
    fn register_gate_never_waits_and_releases_on_guard_drop() {
        let gate = UartRegisterGate::new(());
        let owner = gate.try_enter().expect("first register owner");

        assert!(gate.try_enter().is_none());
        drop(owner);
        assert!(gate.try_enter().is_some());
    }

    #[test]
    fn register_guard_does_not_authorize_an_unrelated_uart() {
        static FIRST_UART_WRITES: AtomicUsize = AtomicUsize::new(0);
        static SECOND_UART_WRITES: AtomicUsize = AtomicUsize::new(0);

        FIRST_UART_WRITES.store(0, Ordering::Relaxed);
        SECOND_UART_WRITES.store(0, Ordering::Relaxed);
        let first_uart_gate = UartRegisterGate::new(RecordingEmergencyTx(&FIRST_UART_WRITES));
        let _second_uart_gate = UartRegisterGate::new(RecordingEmergencyTx(&SECOND_UART_WRITES));
        let first_uart_access = first_uart_gate.try_enter().expect("first UART access");

        assert_eq!(first_uart_access.try_write(b"x"), 1);
        assert_eq!(FIRST_UART_WRITES.load(Ordering::Relaxed), 1);
        assert_eq!(SECOND_UART_WRITES.load(Ordering::Relaxed), 0);
    }
}
