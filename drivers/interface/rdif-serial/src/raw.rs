use core::{
    marker::PhantomData,
    sync::atomic::{AtomicU8, Ordering},
};

use crate::{Config, ConfigError, RxSample, SerialEventSet, SerialIrqReport};

/// Non-blocking exclusion for aliases of one UART register block.
///
/// Normal task and IRQ endpoints use same-CPU IRQ exclusion. This additional
/// gate serializes cross-CPU normal and emergency register access. Emergency
/// takeover is terminal, but each emergency pass still obtains exclusive
/// access so two fatal writers cannot touch the registers concurrently.
pub struct UartRegisterGate<E: ?Sized = dyn UartEmergencyTx> {
    owner: AtomicU8,
    emergency_tx: E,
}

const REGISTER_OWNER_NONE: u8 = 0;
const REGISTER_OWNER_NORMAL: u8 = 1;
const REGISTER_OWNER_EMERGENCY_IDLE: u8 = 2;
const REGISTER_OWNER_EMERGENCY_ACTIVE: u8 = 3;

impl<E> UartRegisterGate<E> {
    /// Creates a free register gate that owns `emergency_tx`.
    pub const fn new(emergency_tx: E) -> Self {
        Self {
            owner: AtomicU8::new(REGISTER_OWNER_NONE),
            emergency_tx,
        }
    }
}

impl<E: ?Sized> UartRegisterGate<E> {
    /// Attempts to claim the register block without waiting.
    pub fn try_enter(&self) -> Option<UartRegisterGuard<'_, E>> {
        self.owner
            .compare_exchange(
                REGISTER_OWNER_NONE,
                REGISTER_OWNER_NORMAL,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .ok()
            .map(|_| UartRegisterGuard {
                gate: self,
                _not_send: PhantomData,
            })
    }

    /// Returns whether emergency output permanently owns the register block.
    pub fn emergency_active(&self) -> bool {
        matches!(
            self.owner.load(Ordering::Acquire),
            REGISTER_OWNER_EMERGENCY_IDLE | REGISTER_OWNER_EMERGENCY_ACTIVE
        )
    }
}

impl<E: UartEmergencyTx + ?Sized> UartRegisterGate<E> {
    /// Claims the terminal emergency endpoint and quiesces this UART.
    ///
    /// The first successful claim permanently excludes normal task and IRQ
    /// endpoints. Every successful claim masks all device-local interrupt
    /// sources before returning a writable access proof. Later emergency
    /// claims remain possible, but concurrent emergency register access is
    /// rejected.
    pub fn try_begin_emergency(&self) -> Option<UartEmergencyAccess<'_, E>> {
        let owner = self.owner.load(Ordering::Acquire);
        if owner != REGISTER_OWNER_NONE && owner != REGISTER_OWNER_EMERGENCY_IDLE {
            return None;
        }
        self.owner
            .compare_exchange(
                owner,
                REGISTER_OWNER_EMERGENCY_ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()?;
        // SAFETY: the successful state transition excludes every normal and
        // emergency alias. Ownership is terminal even if masking itself cannot
        // make forward progress, so normal endpoints can never reappear.
        unsafe { self.emergency_tx.mask_interrupts_unlocked() };
        Some(UartEmergencyAccess {
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

impl<E: ?Sized> Drop for UartRegisterGuard<'_, E> {
    fn drop(&mut self) {
        self.gate
            .owner
            .store(REGISTER_OWNER_NONE, Ordering::Release);
    }
}

/// Proof that fatal output permanently owns one UART register block.
///
/// Dropping this value does not return ownership to normal endpoints. A later
/// emergency formatting call may obtain another access proof from the same
/// gate, while all normal transactions remain excluded until shutdown.
pub struct UartEmergencyAccess<'a, E: ?Sized = dyn UartEmergencyTx> {
    gate: &'a UartRegisterGate<E>,
    _not_send: PhantomData<*mut ()>,
}

impl<E: ?Sized> Drop for UartEmergencyAccess<'_, E> {
    fn drop(&mut self) {
        self.gate
            .owner
            .store(REGISTER_OWNER_EMERGENCY_IDLE, Ordering::Release);
    }
}

impl<E: UartEmergencyTx + ?Sized> UartEmergencyAccess<'_, E> {
    /// Performs one bounded emergency write through this guard's UART.
    pub fn try_write(&self, bytes: &[u8]) -> usize {
        // SAFETY: this access proof is created only after the gate permanently
        // excludes every normal alias of the UART register block.
        unsafe { self.gate.emergency_tx.try_write_unlocked(bytes) }
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
pub struct SerialParts<C, I, E> {
    /// Task-context data and control endpoint.
    pub control: C,
    /// Hard-IRQ event and bounded-RX endpoint.
    pub irq: I,
    /// Panic-only non-blocking transmitter endpoint.
    pub emergency_tx: E,
}

impl<C, I, E> SerialParts<C, I, E> {
    /// Creates a set of disjoint runtime endpoints.
    pub const fn new(control: C, irq: I, emergency_tx: E) -> Self {
        Self {
            control,
            irq,
            emergency_tx,
        }
    }
}

/// Converts a concrete UART into disjoint runtime endpoints.
pub trait SplitUart: Sized {
    type Control: UartPort;
    type Irq: UartIrq;
    type EmergencyTx: UartEmergencyTx;

    fn runtime_info(&self) -> UartInfo;

    fn split(self) -> SerialParts<Self::Control, Self::Irq, Self::EmergencyTx>;
}

/// UART data/control endpoint owned by one runtime maintenance task.
///
/// All calls must run on the same CPU as the associated [`UartIrq`] with local
/// device IRQ delivery excluded. This is a device-serialization contract, not
/// a memory-safety precondition.
pub trait UartPort: Send + 'static {
    /// Initializes the UART while leaving every device interrupt source masked.
    ///
    /// On error, implementations must restore the configuration registers they
    /// changed so a transactional early-console handoff can safely roll back.
    /// [`ConfigError::RegisterError`] is reserved for failures where hardware
    /// state cannot be proven restored and the caller must fail closed.
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError>;

    fn shutdown(&mut self);

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError>;

    /// Reads one normalized hardware sample without consulting IRQ state.
    fn read_rx(&mut self) -> Option<RxSample>;

    /// Discards bytes and error state pending in the hardware receiver.
    fn discard_rx(&mut self);

    /// Writes as much of `bytes` as the hardware can currently accept.
    fn write_tx(&mut self, bytes: &[u8]) -> usize;

    /// Discards bytes queued in the hardware transmitter. A byte already in
    /// the shift register may still complete.
    /// Returns whether the hardware supports an independent TX-only discard.
    fn discard_tx(&mut self) -> bool;

    /// Returns whether both the FIFO and transmitter shift register are empty.
    fn tx_idle(&mut self) -> bool;

    /// Masks only the requested device-local interrupt sources.
    fn mask(&mut self, sources: SerialEventSet);

    fn mask_all(&mut self);

    /// Rearms `sources` and closes the enable/readiness race.
    ///
    /// Sources that are already ready after being enabled are masked again and
    /// returned so the maintenance task can immediately continue servicing
    /// them instead of relying on a possibly lost edge.
    fn rearm(&mut self, sources: SerialEventSet) -> SerialEventSet;
}

/// UART hard-IRQ endpoint owned by the registered IRQ callback.
pub trait UartIrq: Send + 'static {
    /// Masks only the requested device-local interrupt sources.
    ///
    /// This must not disable or otherwise modify a shared interrupt-controller
    /// line. It is used when the runtime queue becomes full after the driver
    /// has already returned a report.
    fn mask(&mut self, sources: SerialEventSet);

    /// Handles the current hardware event and returns a bounded value report.
    ///
    /// `None` means the shared interrupt was not raised by this UART. The
    /// implementation must never call runtime code or write TX FIFO data.
    fn handle(&mut self) -> Option<SerialIrqReport>;
}

/// Raw panic/emergency-only TX endpoint.
///
/// This endpoint deliberately exposes no configuration, interrupt, RX, or
/// blocking operation. A runtime must move it into [`UartRegisterGate`] before
/// exposing safe emergency output.
pub trait UartEmergencyTx: Send + Sync + 'static {
    /// Permanently masks every device-local interrupt source.
    ///
    /// [`UartRegisterGate::try_begin_emergency`] invokes this before it returns
    /// a safe writer. It must leave the UART quiesced between formatting
    /// passes, and must not disable or otherwise modify a shared
    /// interrupt-controller line.
    ///
    /// # Safety
    ///
    /// The caller must exclusively own every alias of this UART register
    /// block, and normal ownership must never resume afterward.
    unsafe fn mask_interrupts_unlocked(&self);

    /// Performs one bounded pass over currently available FIFO capacity.
    ///
    /// [`Self::mask_interrupts_unlocked`] must have permanently quiesced every
    /// device-local source before this method is called.
    ///
    /// # Safety
    ///
    /// The caller must exclusively own every alias of this UART register
    /// block for the duration of the call. Normal users must call
    /// [`UartEmergencyAccess::try_write`] instead.
    unsafe fn try_write_unlocked(&self, bytes: &[u8]) -> usize;
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicUsize;

    use super::*;

    struct RecordingEmergencyTx {
        writes: &'static AtomicUsize,
        masks: &'static AtomicUsize,
    }

    impl UartEmergencyTx for RecordingEmergencyTx {
        unsafe fn mask_interrupts_unlocked(&self) {
            self.masks.fetch_add(1, Ordering::Relaxed);
        }

        unsafe fn try_write_unlocked(&self, bytes: &[u8]) -> usize {
            self.writes.fetch_add(bytes.len(), Ordering::Relaxed);
            bytes.len()
        }
    }

    struct NoopEmergencyTx;

    impl UartEmergencyTx for NoopEmergencyTx {
        unsafe fn mask_interrupts_unlocked(&self) {}

        unsafe fn try_write_unlocked(&self, _bytes: &[u8]) -> usize {
            0
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
    fn emergency_takeover_persists_between_formatting_calls() {
        static UART_WRITES: AtomicUsize = AtomicUsize::new(0);
        static UART_MASKS: AtomicUsize = AtomicUsize::new(0);

        UART_WRITES.store(0, Ordering::Relaxed);
        UART_MASKS.store(0, Ordering::Relaxed);
        let gate = UartRegisterGate::new(RecordingEmergencyTx {
            writes: &UART_WRITES,
            masks: &UART_MASKS,
        });
        let first = gate.try_begin_emergency().expect("emergency takeover");
        assert_eq!(UART_MASKS.load(Ordering::Relaxed), 1);
        assert_eq!(first.try_write(b"panic"), 5);
        assert!(gate.try_begin_emergency().is_none());
        drop(first);

        assert!(gate.emergency_active());
        assert!(gate.try_enter().is_none());
        let second = gate
            .try_begin_emergency()
            .expect("continued emergency access");
        assert_eq!(UART_MASKS.load(Ordering::Relaxed), 2);
        assert_eq!(second.try_write(b" backtrace"), 10);
        assert_eq!(UART_WRITES.load(Ordering::Relaxed), 15);
    }

    #[test]
    fn emergency_takeover_does_not_steal_an_active_transaction() {
        let gate = UartRegisterGate::new(NoopEmergencyTx);
        let normal = gate.try_enter().expect("normal register owner");

        assert!(gate.try_begin_emergency().is_none());
        drop(normal);
        assert!(gate.try_begin_emergency().is_some());
    }

    #[test]
    fn register_guard_does_not_authorize_an_unrelated_uart() {
        static FIRST_UART_WRITES: AtomicUsize = AtomicUsize::new(0);
        static SECOND_UART_WRITES: AtomicUsize = AtomicUsize::new(0);

        FIRST_UART_WRITES.store(0, Ordering::Relaxed);
        SECOND_UART_WRITES.store(0, Ordering::Relaxed);
        static FIRST_UART_MASKS: AtomicUsize = AtomicUsize::new(0);
        static SECOND_UART_MASKS: AtomicUsize = AtomicUsize::new(0);
        FIRST_UART_MASKS.store(0, Ordering::Relaxed);
        SECOND_UART_MASKS.store(0, Ordering::Relaxed);
        let first_uart_gate = UartRegisterGate::new(RecordingEmergencyTx {
            writes: &FIRST_UART_WRITES,
            masks: &FIRST_UART_MASKS,
        });
        let _second_uart_gate = UartRegisterGate::new(RecordingEmergencyTx {
            writes: &SECOND_UART_WRITES,
            masks: &SECOND_UART_MASKS,
        });
        let first_uart_access = first_uart_gate
            .try_begin_emergency()
            .expect("first UART access");

        assert_eq!(first_uart_access.try_write(b"x"), 1);
        assert_eq!(FIRST_UART_MASKS.load(Ordering::Relaxed), 1);
        assert_eq!(SECOND_UART_MASKS.load(Ordering::Relaxed), 0);
        assert_eq!(FIRST_UART_WRITES.load(Ordering::Relaxed), 1);
        assert_eq!(SECOND_UART_WRITES.load(Ordering::Relaxed), 0);
    }
}
