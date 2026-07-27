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
pub struct UartRegisterGate {
    active: AtomicBool,
}

impl UartRegisterGate {
    pub const fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
        }
    }

    pub fn try_enter(&self) -> Option<UartRegisterGuard<'_>> {
        self.active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| UartRegisterGuard {
                gate: self,
                _not_send: PhantomData,
            })
    }
}

impl Default for UartRegisterGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Move-only proof that the caller exclusively owns UART register access.
pub struct UartRegisterGuard<'a> {
    gate: &'a UartRegisterGate,
    _not_send: PhantomData<*mut ()>,
}

impl Drop for UartRegisterGuard<'_> {
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

/// Panic/emergency-only TX endpoint.
///
/// This endpoint deliberately exposes no configuration, interrupt, RX, or
/// blocking operation. Its required [`UartRegisterGuard`] makes register
/// serialization part of the API rather than a caller-side convention.
/// `try_write` performs one bounded pass over the currently available FIFO
/// capacity and returns immediately.
pub trait UartEmergencyTx: Send + Sync + 'static {
    fn try_write(&self, access: &UartRegisterGuard<'_>, bytes: &[u8]) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_gate_never_waits_and_releases_on_guard_drop() {
        let gate = UartRegisterGate::new();
        let owner = gate.try_enter().expect("first register owner");

        assert!(gate.try_enter().is_none());
        drop(owner);
        assert!(gate.try_enter().is_some());
    }
}
