use alloc::{collections::VecDeque, vec::Vec};

use super::{
    AicError, AicEvent, AicState, ControlState, IoPurpose, LinkState, MailboxState, MonotonicTime,
    PendingIo, SdioRequestKind, StartupState, TxToken,
};
use crate::{common::ChipVariant, profile::ChipProfile, tx::TxState};

pub(super) struct ActiveTx {
    pub completion: TxCompletion,
    pub wire_frame: Vec<u8>,
}

#[derive(Clone, Copy)]
pub(super) enum InternalTxKind {
    M2,
    M4,
}

pub(super) enum TxCompletion {
    User(TxToken),
    Internal(InternalTxKind),
}

pub(super) struct InternalTx {
    pub kind: InternalTxKind,
    pub ethernet_frame: Vec<u8>,
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
    pub receive: ReceiveScan,
    pub last_irq_sequence: u64,
}

pub(super) struct ReceiveScan {
    pub active: bool,
    pub next_path: u8,
}

pub(super) struct DataPlaneState {
    pub events: VecDeque<AicEvent>,
    pub tx: TxState,
    pub active_tx: Option<ActiveTx>,
    pub internal_tx: VecDeque<InternalTx>,
    pub link: LinkState,
}

/// Sole owner of all AIC protocol and data-plane state.
pub struct AicDevice {
    pub(super) profile: &'static ChipProfile,
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
        let profile = ChipProfile::for_variant(chip).ok_or(AicError::UnsupportedChip)?;
        Ok(Self {
            profile,
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
                receive: ReceiveScan {
                    active: false,
                    next_path: 0,
                },
                last_irq_sequence: 0,
            },
            data: DataPlaneState {
                events: VecDeque::new(),
                tx: TxState::new(),
                active_tx: None,
                internal_tx: VecDeque::new(),
                link: LinkState::new(),
            },
        })
    }

    /// Returns the externally visible lifecycle state.
    pub const fn state(&self) -> AicState {
        self.lifecycle.state
    }

    /// Returns the selected chip variant.
    pub const fn chip(&self) -> ChipVariant {
        self.profile.variant()
    }

    /// Returns the MAC address learned during startup.
    pub const fn mac_address(&self) -> [u8; 6] {
        match self.data.link.mac_address() {
            Some(mac) => mac,
            None => [0; 6],
        }
    }

    /// Whether the level-sensitive SDIO CARD_INT source is needed by the
    /// current protocol phase. Firmware confirmations during startup and all
    /// data/control work after Ready use this source; unrelated startup phases
    /// keep it masked so stale levels cannot cause a receive-scan storm.
    #[cfg(any(feature = "rdif", test))]
    pub(crate) fn card_irq_needed(&self) -> bool {
        self.lifecycle.state == AicState::Ready || self.startup_confirmation_waiting()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_has_one_supported_dual_function_profile() {
        let device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();

        assert_eq!(device.chip(), ChipVariant::Aic8800DC);
        assert_eq!(device.command_function(), 2);
        assert!(!device.transport_uses_header_crc());
    }

    #[test]
    fn unsupported_variants_do_not_fall_back_to_the_dc_profile() {
        for variant in [
            ChipVariant::Aic8801,
            ChipVariant::Aic8800DW,
            ChipVariant::Aic8800D80X2,
            ChipVariant::Unknown,
        ] {
            assert!(matches!(
                AicDevice::new(variant),
                Err(AicError::UnsupportedChip)
            ));
        }
    }

    #[test]
    fn card_interrupt_is_needed_only_for_ready_or_startup_confirmation() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        assert!(!device.card_irq_needed());

        device.lifecycle.state = AicState::Starting;
        assert!(!device.card_irq_needed());
        device.lifecycle.mailbox = Some(MailboxState::confirmation_for_test(
            MonotonicTime::from_nanos(10),
        ));
        assert!(device.card_irq_needed());

        device.lifecycle.state = AicState::Ready;
        device.lifecycle.mailbox = None;
        assert!(device.card_irq_needed());
    }
}
