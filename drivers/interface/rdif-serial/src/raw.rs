use crate::{Config, ConfigError, RxSample, SerialEventSet, SerialIrqEvent};

/// Immutable information reported by a concrete UART before it is split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UartInfo {
    pub name: &'static str,
    pub register_base: usize,
    pub initial_baudrate: u32,
}

/// Independently owned task-side and hard-IRQ endpoints.
pub struct UartParts<P, I> {
    pub port: P,
    pub irq: I,
}

impl<P, I> UartParts<P, I> {
    pub const fn new(port: P, irq: I) -> Self {
        Self { port, irq }
    }
}

/// Converts a concrete UART into disjoint runtime endpoints.
pub trait SplitUart: Sized {
    type Port: UartPort;
    type Irq: UartIrq;

    fn runtime_info(&self) -> UartInfo;

    fn split(self) -> UartParts<Self::Port, Self::Irq>;
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
