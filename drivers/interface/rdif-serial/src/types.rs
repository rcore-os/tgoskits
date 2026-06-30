use bitflags::bitflags;

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
    pub irq_budget_exhausted: u64,
    pub rx_bytes: u64,
    pub rx_fifo_overruns: u64,
    pub rx_queue_dropped: u64,
    pub rx_breaks: u64,
    pub rx_parity_errors: u64,
    pub rx_framing_errors: u64,
    pub tx_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialIrqOutcome {
    pub claimed: bool,
    pub rx_pushed: usize,
    pub tx_sent: usize,
    pub tx_wakeup: bool,
    pub budget_exhausted: bool,
}
