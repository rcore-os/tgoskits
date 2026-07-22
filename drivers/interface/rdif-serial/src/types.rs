use bitflags::bitflags;

bitflags! {
    /// Stable event classes exchanged between a UART and its runtime.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct SerialEventSet: u32 {
        const RX_DATA      = 1 << 0;
        const RX_TIMEOUT   = 1 << 1;
        const RX_STATUS    = 1 << 2;
        const TX_SPACE     = 1 << 3;
        const MODEM_STATUS = 1 << 4;
        const BUSY_DETECT  = 1 << 5;
        const FAULT        = 1 << 6;

        const RX = Self::RX_DATA.bits() | Self::RX_TIMEOUT.bits() | Self::RX_STATUS.bits();
    }
}

impl SerialEventSet {
    pub const fn has_rx(self) -> bool {
        self.intersects(Self::RX)
    }

    pub const fn has_tx(self) -> bool {
        self.contains(Self::TX_SPACE)
    }
}

bitflags! {
    /// RX error state captured without consuming FIFO data.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct RxErrorFlags: u32 {
        const BREAK   = 1 << 0;
        const PARITY  = 1 << 1;
        const FRAMING = 1 << 2;
        const OVERRUN = 1 << 3;
    }
}

/// Stable event produced by an IRQ-owned UART endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialIrqEvent {
    pub events: SerialEventSet,
    pub rx_errors: RxErrorFlags,
    /// Sources masked by the IRQ endpoint and awaiting task-side rearm.
    pub rearm: SerialEventSet,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RxFlag {
    #[default]
    Normal,
    Break,
    Parity,
    Framing,
}

/// One hardware receive sample. Runtime channel policy is intentionally absent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RxSample {
    pub byte: Option<u8>,
    pub flag: RxFlag,
    pub overrun: bool,
}
