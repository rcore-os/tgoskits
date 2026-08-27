use alloc::{collections::VecDeque, vec::Vec};

use super::{
    AicError, AicEvent, AicState, ControlState, IoPurpose, MailboxState, MonotonicTime, PendingIo,
    SdioRequestKind, StartupState, TxToken,
};
use crate::{
    common::ChipVariant, firmware::images, registers::RegisterMap, rx::RxState, tx::TxState,
};

pub(super) struct ActiveTx {
    pub token: TxToken,
    pub wire_frame: Vec<u8>,
}

pub(super) struct LifecycleState {
    pub state: AicState,
    pub startup: Option<StartupState>,
    pub mailbox: Option<MailboxState>,
    pub control: Option<ControlState>,
    pub last_time: MonotonicTime,
    pub retry_at: Option<MonotonicTime>,
    pub cancel_pending: bool,
}

pub(super) struct IoState {
    pub pending: Option<PendingIo>,
    pub next: Option<(IoPurpose, SdioRequestKind)>,
    pub next_request_id: u64,
    pub irq_pending: bool,
    pub last_irq_sequence: u64,
}

pub(super) struct DataPlaneState {
    pub events: VecDeque<AicEvent>,
    pub rx: RxState,
    pub tx: TxState,
    pub active_tx: Option<ActiveTx>,
    pub mac_address: [u8; 6],
    pub interface_index: u8,
    pub station_index: u8,
}

/// Sole owner of all AIC protocol and data-plane state.
pub struct AicDevice {
    pub(super) chip: ChipVariant,
    pub(super) registers: RegisterMap,
    pub(super) lifecycle: LifecycleState,
    pub(super) io: IoState,
    pub(super) data: DataPlaneState,
}

impl AicDevice {
    /// Creates a stopped device owner for one supported chip.
    ///
    /// # Errors
    ///
    /// Returns [`AicError::UnsupportedChip`] when no firmware image and startup
    /// sequence exist for `chip`.
    pub fn new(chip: ChipVariant) -> Result<Self, AicError> {
        images(chip).ok_or(AicError::UnsupportedChip)?;
        Ok(Self {
            chip,
            registers: RegisterMap::for_chip(chip),
            lifecycle: LifecycleState {
                state: AicState::Stopped,
                startup: None,
                mailbox: None,
                control: None,
                last_time: MonotonicTime::default(),
                retry_at: None,
                cancel_pending: false,
            },
            io: IoState {
                pending: None,
                next: None,
                next_request_id: 1,
                irq_pending: false,
                last_irq_sequence: 0,
            },
            data: DataPlaneState {
                events: VecDeque::new(),
                rx: RxState::new(),
                tx: TxState::new(),
                active_tx: None,
                mac_address: [0; 6],
                interface_index: 0xff,
                station_index: 0xff,
            },
        })
    }

    /// Returns the externally visible lifecycle state.
    pub const fn state(&self) -> AicState {
        self.lifecycle.state
    }

    /// Returns the selected chip variant.
    pub const fn chip(&self) -> ChipVariant {
        self.chip
    }

    /// Returns the MAC address learned during startup.
    pub const fn mac_address(&self) -> [u8; 6] {
        self.data.mac_address
    }
}
