use bitflags::bitflags;

pub type SerialIrqCapture = rdif_irq::IrqCapture<SerialIrqEvent, SerialIrqFault>;

bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct InterruptMask: u32 {
        const RX_DATA      = 1 << 0;
        const RX_STATUS    = 1 << 1;
        const TX_SPACE     = 1 << 2;
        const MODEM_STATUS = 1 << 3;

        const RX = Self::RX_DATA.bits() | Self::RX_STATUS.bits();
        const RX_AVAILABLE = Self::RX.bits();
        const TX_EMPTY = Self::TX_SPACE.bits();
    }
}

impl InterruptMask {
    pub fn rx_available(&self) -> bool {
        self.intersects(Self::RX)
    }

    pub fn tx_empty(&self) -> bool {
        self.contains(Self::TX_SPACE)
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct IrqSource: u32 {
        const RX_DATA      = 1 << 0;
        const RX_TIMEOUT   = 1 << 1;
        const RX_STATUS    = 1 << 2;
        const TX_SPACE     = 1 << 3;
        const MODEM_STATUS = 1 << 4;
        const BUSY_DETECT  = 1 << 5;
        const OTHER_ACK    = 1 << 6;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrqSnapshot {
    pub claimed: bool,
    pub sources: IrqSource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RxFlag {
    #[default]
    Normal,
    Break,
    Parity,
    Framing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RxSample {
    pub byte: Option<u8>,
    pub flag: RxFlag,
    pub overrun: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxItem {
    Byte { byte: u8, flag: RxFlag },
    Overrun,
}

impl Default for RxItem {
    fn default() -> Self {
        Self::Byte {
            byte: 0,
            flag: RxFlag::Normal,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialCounters {
    pub irq_total: u64,
    pub irq_spurious: u64,
    pub service_budget_exhausted: u64,
    pub rx_bytes: u64,
    pub rx_fifo_overruns: u64,
    pub rx_queue_dropped: u64,
    pub rx_breaks: u64,
    pub rx_parity_errors: u64,
    pub rx_framing_errors: u64,
    pub tx_bytes: u64,
}

/// Pure notification and accounting facts produced by one bounded service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialIrqEvents {
    pub rx_pushed: usize,
    pub tx_sent: usize,
    pub tx_wakeup: bool,
}

/// Stable device status captured and acknowledged by the hard-IRQ endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialIrqEvent {
    sources: IrqSource,
}

impl SerialIrqEvent {
    pub(crate) const fn new(sources: IrqSource) -> Self {
        Self { sources }
    }

    /// Raw, already-acknowledged source facts for diagnostics and routing.
    pub const fn sources(self) -> IrqSource {
        self.sources
    }

    /// Whether the maintenance owner must consume a masked-source token.
    pub fn requires_owner_service(self) -> bool {
        self.sources.intersects(
            IrqSource::RX_DATA | IrqSource::RX_TIMEOUT | IrqSource::RX_STATUS | IrqSource::TX_SPACE,
        )
    }
}

/// Result of one owner-side pass over stable device-masked sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "masked UART sources require an explicit rearm or another pass"]
pub enum SerialMaskedService {
    Complete(SerialIrqEvents),
    Pending(SerialIrqEvents),
    Backpressured(SerialIrqEvents),
    Fault(SerialIrqFault),
    Stale,
}

/// Fail-closed condition that disabled the portable UART interrupt source.
#[derive(thiserror::Error, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerialIrqFault {
    /// The raw endpoint claimed an IRQ without a serviceable source.
    #[error("raw UART claimed an IRQ without a serviceable source")]
    UnknownSource,
    /// Budget exhaustion involved a source without a safe device-local mask.
    #[error("UART IRQ source cannot be safely masked")]
    UnmaskableSource,
    /// The raw endpoint reported an invalid zero-byte transmit load.
    #[error("raw UART reported an invalid zero-byte transmit load")]
    InvalidTransmitLoad,
}

/// Rejection of an explicit source rearm against parent-owned masked state.
#[derive(thiserror::Error, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerialRearmError {
    #[error("serial IRQ event belongs to a stale runtime generation")]
    Stale,
    #[error("serial IRQ source was not masked by this event")]
    NotCaptured,
    #[error("serial RX remains backpressured")]
    RxBackpressured,
}

/// Rejection of the final device-source enable transition.
#[derive(thiserror::Error, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerialActivationError {
    #[error("serial port was not prepared for interrupt activation")]
    NotPrepared,
}
