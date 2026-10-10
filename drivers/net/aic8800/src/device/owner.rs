use alloc::{collections::VecDeque, vec::Vec};

use super::{
    AicError, AicEvent, AicState, ControlState, IoPurpose, LinkState, MailboxState, MonotonicTime,
    PendingIo, SdioRequestKind, StartupState, TxToken,
};
use crate::{
    common::ChipVariant,
    profile::ChipProfile,
    protocol::BLOCK_SIZE,
    rx::{RX_BYTE_CAPACITY, RX_CAPACITY},
    tx::TxState,
};

pub(super) struct ActiveTx {
    pub completion: TxCompletion,
    /// The frames this write carries, laid out the way the firmware walks
    /// them: one frame after another, each as long as its own declared length
    /// rounded up to the transmit alignment.
    pub wire_frame: Vec<u8>,
    /// Stream length inside `wire_frame`. Everything past it is the block
    /// padding the write form ends with, which the firmware reads as the end
    /// of the stream and which a further frame replaces.
    pub stream_len: usize,
    pub retry_at: Option<MonotonicTime>,
    /// Times this write has waited for the firmware pool to drain instead of
    /// committing a batch the pool cannot back. Bounded so a pool that stays
    /// empty cannot stall the transmit path.
    pub credit_waits: u32,
    /// Tokens of the packets this write carries beyond the first.
    pub extra_tokens: Vec<TxToken>,
}

impl ActiveTx {
    /// Starts a write that carries one complete wire frame.
    pub(super) fn new(completion: TxCompletion, wire_frame: Vec<u8>, stream_len: usize) -> Self {
        debug_assert!(stream_len <= wire_frame.len());
        Self {
            completion,
            wire_frame,
            stream_len,
            retry_at: None,
            credit_waits: 0,
            extra_tokens: Vec::new(),
        }
    }

    /// Packets this write carries.
    pub(super) fn packets(&self) -> usize {
        1 + self.extra_tokens.len()
    }

    /// Appends one more validated frame, dropping padding after the previous
    /// frame before extending the stream.
    pub(super) fn append_frame(&mut self, frame: &[u8], stream_len: usize) {
        debug_assert!(stream_len <= frame.len());
        self.wire_frame.truncate(self.stream_len);
        self.wire_frame.extend_from_slice(&frame[..stream_len]);
        self.stream_len += stream_len;
    }

    /// The bytes one CMD53 carries: the frame stream padded to whole SDIO blocks.
    pub(super) fn wire_bytes(&self) -> Vec<u8> {
        padded_wire_write(&self.wire_frame[..self.stream_len])
    }
}

/// Pads one write to whole SDIO blocks. The padding only ever follows the last
/// frame, where it reads as a zero length; an already block-aligned stream needs
/// none.
fn padded_wire_write(frame: &[u8]) -> Vec<u8> {
    let mut bytes = frame.to_vec();
    let padding = (BLOCK_SIZE - bytes.len() % BLOCK_SIZE) % BLOCK_SIZE;
    bytes.resize(bytes.len() + padding, 0);
    bytes
}

#[derive(Clone, Copy, Eq, PartialEq)]
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
    /// Whether a write completion has already continued the transmit pipeline
    /// since the drive path last ran. Continuations are bounded to one per
    /// drive pass so a saturated transmitter cannot keep the receive side from
    /// draining the firmware, whose buffers are the transmit credit pool.
    pub chain_used: bool,
}

pub(super) struct ReceiveScan {
    pub active: bool,
    pub next_path: u8,
}

pub(super) struct DataPlaneState {
    pub events: VecDeque<AicEvent>,
    pub event_bytes: usize,
    pub tx: TxState,
    pub active_tx: Option<ActiveTx>,
    /// Firmware packet buffers the last flow-control reading granted, minus the
    /// data writes completed since. `None` means the next transmit must re-read
    /// the register: the value is dropped when it reaches the command reserve
    /// and whenever command traffic or cancellation may have moved the shared
    /// pool.
    pub tx_credits: Option<u8>,
    pub internal_tx: VecDeque<InternalTx>,
    pub internal_tx_bytes: usize,
    pub link: LinkState,
    /// Transmit completions that did not fit the event queue.
    pub pending_completions: VecDeque<TxToken>,
}

impl DataPlaneState {
    pub(super) fn push_event(&mut self, event: AicEvent) -> Result<(), AicError> {
        let event_bytes = match &event {
            AicEvent::Receive(frame) => frame.len(),
            _ => 0,
        };
        let is_receive = matches!(event, AicEvent::Receive(_));
        while (self.events.len() >= RX_CAPACITY
            || self.event_bytes.saturating_add(event_bytes) > RX_BYTE_CAPACITY)
            && !is_receive
        {
            let Some(index) = self
                .events
                .iter()
                .position(|queued| matches!(queued, AicEvent::Receive(_)))
            else {
                break;
            };
            self.remove_event(index);
        }
        if self.events.len() >= RX_CAPACITY
            || self.event_bytes.saturating_add(event_bytes) > RX_BYTE_CAPACITY
        {
            // Data frames are lossy at this boundary. Dropping them keeps
            // firmware-controlled traffic from growing the owner's memory.
            return if is_receive {
                Ok(())
            } else {
                Err(AicError::EventQueueFull)
            };
        }
        self.event_bytes += event_bytes;
        self.events.push_back(event);
        Ok(())
    }

    pub(super) fn pop_event(&mut self) -> Option<AicEvent> {
        let event = self.events.pop_front()?;
        self.event_bytes -= event_payload_bytes(&event);
        Some(event)
    }

    pub(super) fn remove_event(&mut self, index: usize) -> Option<AicEvent> {
        let event = self.events.remove(index)?;
        self.event_bytes -= event_payload_bytes(&event);
        Some(event)
    }

    pub(super) fn pop_internal_tx(&mut self) -> Option<InternalTx> {
        let internal = self.internal_tx.pop_front()?;
        self.internal_tx_bytes -= internal.ethernet_frame.len();
        Some(internal)
    }

    pub(super) fn clear_internal_tx(&mut self) {
        self.internal_tx.clear();
        self.internal_tx_bytes = 0;
    }

    /// Publishes transmit completions that were held back because the event
    /// queue was full. Aggregated writes complete several packets at once, so
    /// a burst can outrun the queue; deferred tokens are emitted individually.
    pub(super) fn promote_pending_completions(&mut self) {
        while self.event_room() > 0 {
            let Some(token) = self.pending_completions.pop_front() else {
                return;
            };
            if self.push_event(AicEvent::TransmitComplete(token)).is_err() {
                self.pending_completions.push_front(token);
                return;
            }
        }
    }

    /// Room for another event without displacing a queued receive frame.
    pub(super) fn event_room(&self) -> usize {
        RX_CAPACITY.saturating_sub(self.events.len())
    }
}

fn event_payload_bytes(event: &AicEvent) -> usize {
    match event {
        AicEvent::Receive(frame) => frame.len(),
        _ => 0,
    }
}

/// Frames one transmit write may carry and the soft byte target it aims not to
/// exceed. The target is checked before appending a frame, so a write may cross
/// it by at most one frame; it is not a ring-capacity limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxAggregation {
    /// Frames one write may carry.
    pub packets: usize,
    /// Target bytes per write before another frame is appended.
    pub bytes: usize,
}

impl TxAggregation {
    /// Bytes one maximum-length Ethernet packet adds to a write stream: the
    /// 1500-byte payload behind the 28-byte host descriptor and the 4-byte SDIO
    /// header, rounded up to a whole SDIO block so the default target is a whole
    /// number of them.
    pub const MAX_FRAME_BYTES: usize = 1536;
    /// Default target: four maximum-length frame streams.
    pub const DEFAULT_BYTES: usize = 4 * Self::MAX_FRAME_BYTES;

    /// Creates a write aggregation policy. Zero limits are invalid and are
    /// rejected by `AicDevice::set_tx_aggregation` or `AicRdifDevice::new`.
    pub const fn new(packets: usize, bytes: usize) -> Self {
        Self { packets, bytes }
    }

    /// Whether both limits can carry a frame.
    pub const fn is_valid(self) -> bool {
        self.packets > 0 && self.bytes > 0
    }
}

impl Default for TxAggregation {
    fn default() -> Self {
        Self::new(1, Self::DEFAULT_BYTES)
    }
}

/// Sole owner of all AIC protocol and data-plane state.
pub struct AicDevice {
    pub(super) profile: &'static ChipProfile,
    pub(super) lifecycle: LifecycleState,
    pub(super) io: IoState,
    pub(super) data: DataPlaneState,
    /// What one transmit write may carry. A single frame unless the layer
    /// that hands frames over asks for batching.
    pub(super) tx_aggregation: TxAggregation,
}

impl AicDevice {
    /// Sets what one transmit write may carry.
    ///
    /// # Errors
    ///
    /// Returns [`AicError::InvalidTxAggregation`] when either limit is zero.
    pub fn set_tx_aggregation(&mut self, aggregation: TxAggregation) -> Result<(), AicError> {
        if !aggregation.is_valid() {
            return Err(AicError::InvalidTxAggregation);
        }
        self.tx_aggregation = aggregation;
        Ok(())
    }

    /// What one transmit write may carry.
    pub const fn tx_aggregation(&self) -> TxAggregation {
        self.tx_aggregation
    }

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
            tx_aggregation: TxAggregation::default(),
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
                chain_used: false,
            },
            data: DataPlaneState {
                events: VecDeque::new(),
                event_bytes: 0,
                tx: TxState::new(),
                active_tx: None,
                tx_credits: None,
                internal_tx: VecDeque::new(),
                internal_tx_bytes: 0,
                link: LinkState::new(),
                pending_completions: VecDeque::new(),
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
    fn tx_aggregation_rejects_a_zero_limit_instead_of_repairing_it() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();

        for invalid in [TxAggregation::new(0, 4096), TxAggregation::new(4, 0)] {
            assert_eq!(
                device.set_tx_aggregation(invalid),
                Err(AicError::InvalidTxAggregation),
                "a zero limit can never carry a frame or a byte"
            );
        }
        assert_eq!(
            device.tx_aggregation(),
            TxAggregation::default(),
            "a rejected policy leaves the previous one in place"
        );

        assert!(device.set_tx_aggregation(TxAggregation::new(1, 1)).is_ok());
        assert_eq!(device.tx_aggregation(), TxAggregation::new(1, 1));
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
