use alloc::{vec, vec::Vec};
use core::time::Duration;

use super::*;
use crate::{
    lmac::{
        SM_CONNECT_IND, SM_DISCONNECT_IND, parse_connect_indication, parse_disconnect_indication,
    },
    profile::DataTxFlowPolicy,
    protocol::{BLOCK_SIZE, ethernet_tx_frame},
    registers::{ReceiveLength, flow_credits},
    rx::{ParsedFrame, RX_CAPACITY, parse_fifo},
};

const IO_RETRY: Duration = Duration::from_millis(1);
const INTERNAL_TX_CAPACITY: usize = 2;
const ETHERTYPE_EAPOL: [u8; 2] = [0x88, 0x8e];

impl AicDevice {
    pub(super) fn drive_ready(&mut self, now: MonotonicTime) -> AicAction {
        if self.mailbox_timed_out(now) {
            return self.drive_mailbox(now);
        }
        if self.lifecycle.mailbox.is_some() {
            // LMAC confirmations arrive on the command/data FIFO and are
            // announced through the level-sensitive CARD_INT source.  Once a
            // mailbox write has completed, drain one bounded receive scan
            // before waiting for the next interrupt; otherwise a pending
            // confirmation would leave the mailbox parked while the control
            // command at the front of the queue is submitted again.
            if self.mailbox_waiting_for_receive()
                && let Some(action) = self.drive_receive_scan()
            {
                return action;
            }
            return self.drive_mailbox(now);
        }
        if let Some(control) = self.lifecycle.control.as_ref()
            && let Some(command) = control.commands.front()
        {
            let message_id = command.message_id;
            let destination = command.destination;
            let expected = command.expected_message_id;
            let payload = command.payload.clone();
            self.begin_lmac_mailbox(message_id, destination, &payload, expected, now);
            return self.drive_mailbox(now);
        }
        // Deliver terminal/control events before starting another level-triggered
        // receive scan.  CARD_INT may remain asserted while the firmware drains
        // queued traffic; scanning first would indefinitely postpone the
        // ControlComplete event that releases the Linux WEXT caller.
        if let Some(event) = self.take_priority_event() {
            return AicAction::Event(event);
        }
        if let Some(action) = self.drive_receive_scan() {
            return action;
        }
        self.prepare_next_transmit();
        if self.data.active_tx.is_some() {
            return match self.data_tx_flow_policy() {
                DataTxFlowPolicy::Direct => {
                    let wire_frame = self
                        .data
                        .active_tx
                        .as_ref()
                        .expect("active TX is present after preparation")
                        .wire_frame
                        .clone();
                    self.emit(
                        IoPurpose::TransmitData,
                        write_fifo(
                            self.data_function(),
                            self.registers().write_fifo,
                            wire_frame,
                        ),
                    )
                }
                DataTxFlowPolicy::CreditGated => self.emit(
                    IoPurpose::TransmitFlow,
                    read_byte(self.data_function(), self.registers().flow_control),
                ),
            };
        }
        AicAction::WaitForInterrupt
    }

    fn take_priority_event(&mut self) -> Option<AicEvent> {
        let index = self
            .data
            .events
            .iter()
            .position(|event| !matches!(event, AicEvent::Receive(_)))?;
        self.data.events.remove(index)
    }

    pub(super) fn request_receive_scan(&mut self) {
        // Firmware startup owns both SDIO functions until a mailbox
        // confirmation is waiting.  The controller reports CARD_INT as a
        // level source, so treating that status as a data-plane receive event
        // during function setup would continually preempt the startup FSM.
        // The first confirmation interrupt is the sole startup exception; it
        // arms one bounded scan and subsequent level samples are coalesced.
        if self.lifecycle.state == AicState::Starting && !self.startup_confirmation_waiting() {
            return;
        }
        if self.io.receive.active {
            // CARD_INT is level-triggered.  The in-flight scan drains every
            // enabled function; rearm_and_check() observes a source that
            // remains asserted and schedules the next scan after this one
            // completes, so a second scan must never be queued here.
            return;
        }
        self.io.receive.active = true;
        self.io.receive.next_path = 0;
    }

    pub(super) fn drive_receive_scan(&mut self) -> Option<AicAction> {
        if !self.io.receive.active {
            return None;
        }
        if let Some(path) = self.receive_path(usize::from(self.io.receive.next_path)) {
            let function = self.receive_function(path);
            return Some(self.emit(
                IoPurpose::ReceiveCount(path),
                read_byte(function, self.registers().block_count),
            ));
        }
        self.io.receive.active = false;
        self.io.receive.next_path = 0;
        None
    }

    pub(super) fn consume_receive_count(
        &mut self,
        path: RxPath,
        response: SdioResponse,
    ) -> Result<(), AicError> {
        let count = expect_byte(response)?;
        match self.registers().receive_length(count) {
            ReceiveLength::Empty | ReceiveLength::OtherInterrupt => self.advance_receive_path(),
            ReceiveLength::Blocks(blocks) => {
                self.io.next = Some((
                    IoPurpose::ReceiveData(path),
                    read_fifo(
                        self.receive_function(path),
                        self.registers().read_fifo,
                        usize::from(blocks) * BLOCK_SIZE,
                    ),
                ));
            }
            ReceiveLength::ByteMode => {
                self.io.next = Some((
                    IoPurpose::ReceiveByteLength(path),
                    read_byte(
                        self.receive_function(path),
                        self.registers().byte_mode_length,
                    ),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn consume_receive_byte_length(
        &mut self,
        path: RxPath,
        response: SdioResponse,
    ) -> Result<(), AicError> {
        let units = expect_byte(response)?;
        if units == 0 || units > 128 {
            return Err(AicError::InvalidRxByteLength { units });
        }
        self.io.next = Some((
            IoPurpose::ReceiveData(path),
            read_fifo(
                self.receive_function(path),
                self.registers().read_fifo,
                usize::from(units) * 4,
            ),
        ));
        Ok(())
    }

    pub(super) fn consume_receive_data(
        &mut self,
        path: RxPath,
        response: SdioResponse,
    ) -> Result<(), AicError> {
        let receive_data = expect_data(response)?;
        let frames = parse_fifo(&receive_data).map_err(|error| {
            let header_length = receive_data.len().min(24);
            let mut header = [0; 24];
            header[..header_length].copy_from_slice(&receive_data[..header_length]);
            let header_words = [
                u64::from_le_bytes(header[0..8].try_into().expect("fixed header word")),
                u64::from_le_bytes(header[8..16].try_into().expect("fixed header word")),
                u64::from_le_bytes(header[16..24].try_into().expect("fixed header word")),
            ];
            log::error!(
                "malformed AIC RX frame on {path:?}: transfer={} header={:02x?}",
                receive_data.len(),
                &receive_data[..header_length]
            );
            AicError::MalformedRxFrame {
                offset: error.offset,
                packet_type: error.packet_type,
                declared_length: error.declared_length,
                available_length: error.available_length,
                header_words,
            }
        })?;
        for frame in frames {
            match frame {
                ParsedFrame::Data {
                    frame,
                    decryption_status,
                } => {
                    let Some(frames) = decapsulate_data_frames(&frame, decryption_status) else {
                        continue;
                    };
                    for frame in frames {
                        if frame.get(12..14) == Some(&ETHERTYPE_EAPOL) {
                            self.consume_eapol(&frame)?;
                        } else if self.data.events.len() < RX_CAPACITY {
                            self.data.events.push_back(AicEvent::Receive(frame));
                        }
                    }
                }
                ParsedFrame::Confirmation {
                    message_id,
                    payload,
                } => {
                    self.accept_mailbox_confirmation(message_id, payload)?;
                }
                ParsedFrame::DataConfirmation => {
                    log::trace!("AIC firmware data confirmation");
                }
                ParsedFrame::FirmwarePrint { length } => {
                    log::trace!("AIC firmware trace frame: {length} bytes");
                }
                ParsedFrame::Indication {
                    message_id: SM_CONNECT_IND,
                    payload,
                } => {
                    let indication = parse_connect_indication(&payload)?;
                    log::info!(
                        "[wifi] association complete; learned firmware vif={} station={}",
                        indication.interface_index,
                        indication.station_index
                    );
                    self.data.link.install_peer(
                        indication.interface_index,
                        indication.station_index,
                        indication.bssid,
                    )?;
                    let control = self
                        .lifecycle
                        .control
                        .as_mut()
                        .ok_or(AicError::CompletionMismatch)?;
                    let local_mac = self
                        .data
                        .link
                        .mac_address()
                        .ok_or(AicError::InvalidMacAddress)?;
                    control.accept_connect_indication(
                        indication.station_index,
                        indication.bssid,
                        local_mac,
                    )?;
                }
                ParsedFrame::Indication {
                    message_id: SM_DISCONNECT_IND,
                    payload,
                } => {
                    let indication = parse_disconnect_indication(&payload)?;
                    if self.data.link.interface_index() != Some(indication.interface_index) {
                        return Err(AicError::MalformedResponse);
                    }
                    self.data.link.clear_peer();
                    self.data.internal_tx.clear();
                    let resetting = self.lifecycle.control.as_ref().is_some_and(|control| {
                        matches!(&control.operation,
                            super::control::ControlOperation::Connect(connect)
                                if connect.phase == super::control::ConnectPhase::Resetting)
                    });
                    if !resetting && self.lifecycle.control.take().is_some() {
                        self.data.events.push_back(AicEvent::ControlFailed(
                            AicError::Disconnected {
                                reason_code: indication.reason_code,
                            },
                        ));
                    }
                }
                ParsedFrame::Indication {
                    message_id,
                    payload,
                } => {
                    log::trace!(
                        "AIC indication id={message_id:#06x}, payload={} bytes",
                        payload.len()
                    );
                }
            }
        }
        self.io.next = Some((
            IoPurpose::ReceiveCount(path),
            read_byte(self.receive_function(path), self.registers().block_count),
        ));
        Ok(())
    }

    fn advance_receive_path(&mut self) {
        self.io.receive.next_path = self.io.receive.next_path.saturating_add(1);
    }

    pub(super) fn consume_transmit_flow(
        &mut self,
        response: SdioResponse,
        now: MonotonicTime,
    ) -> Result<(), AicError> {
        let credits = flow_credits(expect_byte(response)?);
        let active = self
            .data
            .active_tx
            .as_ref()
            .ok_or(AicError::CompletionMismatch)?;
        if credits == 0 || usize::from(credits) * BLOCK_SIZE <= active.wire_frame.len() {
            self.lifecycle.retry_at = Some(now.after(IO_RETRY));
            return Ok(());
        }
        self.io.next = Some((
            IoPurpose::TransmitData,
            write_fifo(
                self.data_function(),
                self.registers().write_fifo,
                active.wire_frame.clone(),
            ),
        ));
        Ok(())
    }

    pub(super) fn consume_transmit_data(&mut self, response: SdioResponse) -> Result<(), AicError> {
        expect_unit(response)?;
        let active = self
            .data
            .active_tx
            .take()
            .ok_or(AicError::CompletionMismatch)?;
        match active.completion {
            super::owner::TxCompletion::User(token) => self
                .data
                .events
                .push_back(AicEvent::TransmitComplete(token)),
            super::owner::TxCompletion::Internal(super::owner::InternalTxKind::M2) => {}
            super::owner::TxCompletion::Internal(super::owner::InternalTxKind::M4) => {
                let (station_index, _) = self.data.link.peer().ok_or(AicError::WpaProtocol)?;
                self.lifecycle
                    .control
                    .as_mut()
                    .ok_or(AicError::CompletionMismatch)?
                    .accept_m4_transmit(station_index)?;
            }
        }
        Ok(())
    }

    fn prepare_next_transmit(&mut self) {
        if self.data.active_tx.is_some() {
            return;
        }
        let Some((interface_index, station_index)) = self.data.link.tx_indices() else {
            return;
        };
        if let Some(internal) = self.data.internal_tx.pop_front() {
            let Ok(wire_frame) = ethernet_tx_frame(
                &internal.ethernet_frame,
                interface_index,
                station_index,
                self.transport_uses_header_crc(),
            ) else {
                return;
            };
            self.data.active_tx = Some(ActiveTx {
                completion: super::owner::TxCompletion::Internal(internal.kind),
                wire_frame,
            });
            return;
        }
        let Some(frame) = self.data.tx.take_wire_frame(
            interface_index,
            station_index,
            self.transport_uses_header_crc(),
        ) else {
            return;
        };
        match frame {
            Ok((token, wire_frame)) => {
                self.data.active_tx = Some(ActiveTx {
                    completion: super::owner::TxCompletion::User(token),
                    wire_frame,
                });
            }
            Err(token) => self
                .data
                .events
                .push_back(AicEvent::TransmitComplete(token)),
        }
    }

    fn consume_eapol(&mut self, ethernet: &[u8]) -> Result<(), AicError> {
        if ethernet.len() < 14 {
            return Err(AicError::WpaProtocol);
        }
        let local_mac = self
            .data
            .link
            .mac_address()
            .ok_or(AicError::InvalidMacAddress)?;
        let (station_index, bssid) = self.data.link.peer().ok_or(AicError::WpaProtocol)?;
        let interface_index = self
            .data
            .link
            .interface_index()
            .ok_or(AicError::WpaProtocol)?;
        if ethernet[..6] != local_mac || ethernet[6..12] != bssid {
            return Err(AicError::WpaProtocol);
        }
        let effect = self
            .lifecycle
            .control
            .as_mut()
            .ok_or(AicError::WpaProtocol)?
            .process_eapol(interface_index, station_index, &ethernet[14..])?;
        if let super::control::ControlEffect::TransmitEapol(frame) = effect {
            self.queue_internal_eapol(super::owner::InternalTxKind::M2, frame)?;
        }
        Ok(())
    }

    pub(super) fn queue_internal_eapol(
        &mut self,
        kind: super::owner::InternalTxKind,
        eapol: Vec<u8>,
    ) -> Result<(), AicError> {
        if self.data.internal_tx.len() >= INTERNAL_TX_CAPACITY {
            return Err(AicError::TxQueueFull);
        }
        let local_mac = self
            .data
            .link
            .mac_address()
            .ok_or(AicError::InvalidMacAddress)?;
        let (_, bssid) = self.data.link.peer().ok_or(AicError::WpaProtocol)?;
        let mut ethernet = Vec::with_capacity(14 + eapol.len());
        ethernet.extend_from_slice(&bssid);
        ethernet.extend_from_slice(&local_mac);
        ethernet.extend_from_slice(&ETHERTYPE_EAPOL);
        ethernet.extend_from_slice(&eapol);
        self.data.internal_tx.push_back(super::owner::InternalTx {
            kind,
            ethernet_frame: ethernet,
        });
        Ok(())
    }
}

/// Converts the firmware's 802.11 MPDU (after its 60-byte hardware header)
/// into the Ethernet frame expected by the network stack.  The Linux AIC
/// driver performs the same operation in `rwnx_rxdataind_aicwf`: management
/// frames are consumed by the firmware control path, while station data is
/// stripped of its MAC/crypto/LLC headers before delivery.
fn decapsulate_data_frames(frame: &[u8], decryption_status: u8) -> Option<Vec<Vec<u8>>> {
    if frame.len() < 24 {
        return None;
    }
    let frame_control = u16::from_le_bytes([frame[0], frame[1]]);
    if (frame_control >> 2) & 0x3 != 2 {
        return None;
    }
    let to_ds = frame_control & 0x0100 != 0;
    let from_ds = frame_control & 0x0200 != 0;
    let qos = ((frame_control >> 4) & 0x0f) >= 8;
    let has_ht_control = frame_control & 0x8000 != 0;
    let address4_len = usize::from(to_ds && from_ds) * 6;
    let qos_offset = 24 + address4_len;
    let is_amsdu = qos && frame.get(qos_offset).is_some_and(|value| value & 0x80 != 0);
    let header_len = qos_offset + usize::from(qos) * 2 + usize::from(has_ht_control) * 4;
    let crypto_len = match decryption_status {
        0 => 0,
        1 => 4,
        2 | 3 => 8,
        7 => 18,
        // The data path intentionally supports only the cipher suites that
        // the firmware reports to this station driver.  Do not guess a
        // header length for newer/unsupported suites.
        _ => return None,
    };
    let payload = header_len.checked_add(crypto_len)?;
    if frame.len() < payload {
        return None;
    }

    if is_amsdu {
        return decapsulate_amsdu(&frame[payload..]);
    }

    let (destination, source) = match (to_ds, from_ds) {
        (false, false) => (&frame[4..10], &frame[10..16]),
        (true, false) => (&frame[16..22], &frame[10..16]),
        (false, true) => (&frame[4..10], &frame[16..22]),
        (true, true) => (&frame[16..22], &frame[24..30]),
    };
    ethernet_from_llc(destination, source, &frame[payload..]).map(|frame| vec![frame])
}

fn decapsulate_amsdu(aggregate: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while offset < aggregate.len() {
        let header_end = offset.checked_add(14)?;
        if header_end > aggregate.len() {
            return None;
        }
        let msdu_len =
            u16::from_be_bytes([aggregate[offset + 12], aggregate[offset + 13]]) as usize;
        let end = header_end.checked_add(msdu_len)?;
        if msdu_len < 8 || end > aggregate.len() {
            return None;
        }
        frames.push(ethernet_from_llc(
            &aggregate[offset..offset + 6],
            &aggregate[offset + 6..offset + 12],
            &aggregate[header_end..end],
        )?);
        if end == aggregate.len() {
            break;
        }
        let subframe_len = 14usize.checked_add(msdu_len)?;
        let aligned_len = subframe_len.checked_add(3)? & !3;
        offset = offset.checked_add(aligned_len)?;
        if offset >= aggregate.len() {
            return None;
        }
    }
    (!frames.is_empty()).then_some(frames)
}

fn ethernet_from_llc(destination: &[u8], source: &[u8], llc: &[u8]) -> Option<Vec<u8>> {
    if destination.len() != 6
        || source.len() != 6
        || llc.len() < 8
        || llc[..6] != [0xaa, 0xaa, 0x03, 0, 0, 0]
    {
        return None;
    }
    let mut ethernet = Vec::with_capacity(12 + llc.len() - 6);
    ethernet.extend_from_slice(destination);
    ethernet.extend_from_slice(source);
    ethernet.extend_from_slice(&llc[6..]);
    Some(ethernet)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::{
        common::{ChipVariant, SDIO_TYPE_CFG_CMD_RSP, SDIO_TYPE_DATA},
        rx::RX_CAPACITY,
    };

    fn indication_fifo(message_id: u16, payload: &[u8]) -> Vec<u8> {
        let packet_len = 12 + payload.len();
        let mut fifo = vec![0; 4 + packet_len.div_ceil(4) * 4];
        fifo[..2].copy_from_slice(&(packet_len as u16).to_le_bytes());
        fifo[2] = SDIO_TYPE_CFG_CMD_RSP;
        fifo[4..6].copy_from_slice(&message_id.to_le_bytes());
        fifo[10..12].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        fifo[16..16 + payload.len()].copy_from_slice(payload);
        fifo
    }

    fn data_fifo(marker: u8) -> Vec<u8> {
        const FRAME_LENGTH: usize = 24 + 6 + 2 + 1;
        const HARDWARE_HEADER: usize = 60;
        let mut fifo = vec![0; HARDWARE_HEADER + FRAME_LENGTH];
        fifo[..2].copy_from_slice(&(FRAME_LENGTH as u16).to_le_bytes());
        fifo[2] = SDIO_TYPE_DATA;
        let frame = &mut fifo[HARDWARE_HEADER..];
        // AP -> station data MPDU: address 1 is the Ethernet destination,
        // address 2 is the transmitter/source, followed by LLC/SNAP.
        frame[..2].copy_from_slice(&0x0208u16.to_le_bytes());
        frame[4] = marker;
        frame[5..10].copy_from_slice(&[0x10, 0x11, 0x12, 0x13, 0x14]);
        frame[10..16].copy_from_slice(&[0x20, 0x21, 0x22, 0x23, 0x24, 0x25]);
        frame[16..22].copy_from_slice(&[0x30, 0x31, 0x32, 0x33, 0x34, 0x35]);
        frame[24..30].copy_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0]);
        frame[30..32].copy_from_slice(&[0x08, 0x00]);
        frame[32] = marker;
        fifo
    }

    fn data_mpdu(frame_control: u16, payload: &[u8]) -> Vec<u8> {
        let qos = ((frame_control >> 4) & 0x0f) >= 8;
        let header_len = 24 + usize::from(qos) * 2;
        let mut frame = vec![0; header_len + 8 + payload.len()];
        frame[..2].copy_from_slice(&frame_control.to_le_bytes());
        frame[4..10].copy_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        frame[10..16].copy_from_slice(&[0x11, 0x12, 0x13, 0x14, 0x15, 0x16]);
        frame[16..22].copy_from_slice(&[0x21, 0x22, 0x23, 0x24, 0x25, 0x26]);
        if qos {
            frame[24..26].copy_from_slice(&0u16.to_le_bytes());
        }
        frame[header_len..header_len + 6].copy_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0]);
        frame[header_len + 6..header_len + 8].copy_from_slice(&[0x08, 0x00]);
        frame[header_len + 8..].copy_from_slice(payload);
        frame
    }

    #[test]
    fn successful_connect_indication_publishes_firmware_vif_and_station_indices() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Ready;
        device.data.link.install_mac([2, 0, 0, 0, 0, 1]).unwrap();
        device.data.link.install_interface(2).unwrap();
        let mut control = super::super::control::build(
            ControlRequest::Connect {
                ssid: b"network".to_vec(),
                pmk: None,
                entropy: None,
            },
            [2, 0, 0, 0, 0, 1],
            Some(2),
        )
        .unwrap();
        if let super::super::control::ControlOperation::Connect(connect) = &mut control.operation {
            connect.phase = super::super::control::ConnectPhase::AwaitIndication;
        }
        control.commands.clear();
        device.lifecycle.control = Some(control);
        let mut payload = vec![0; 11];
        payload[2..8].copy_from_slice(&[2, 1, 2, 3, 4, 5]);
        payload[9] = 2;
        payload[10] = 7;
        let fifo = indication_fifo(SM_CONNECT_IND, &payload);

        device
            .consume_receive_data(RxPath::Command, SdioResponse::Data(fifo))
            .unwrap();

        assert_eq!(device.data.link.tx_indices(), Some((2, 7)));
    }

    #[test]
    fn asynchronous_disconnect_clears_the_learned_peer() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Ready;
        device.data.link.install_mac([2, 0, 0, 0, 0, 1]).unwrap();
        device.data.link.install_interface(2).unwrap();
        device
            .data
            .link
            .install_peer(2, 7, [2, 1, 2, 3, 4, 5])
            .unwrap();
        let payload = [3, 0, 2, 0, 0, 0];

        device
            .consume_receive_data(
                RxPath::Command,
                SdioResponse::Data(indication_fifo(crate::lmac::SM_DISCONNECT_IND, &payload)),
            )
            .unwrap();

        assert_eq!(device.data.link.peer(), None);
    }

    #[test]
    fn receive_events_do_not_stall_after_the_first_bounded_window() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Ready;

        for marker in 0..=RX_CAPACITY {
            device
                .consume_receive_data(RxPath::Command, SdioResponse::Data(data_fifo(marker as u8)))
                .unwrap();
            let event = device.data.events.pop_front();
            assert!(
                matches!(event, Some(AicEvent::Receive(frame)) if frame[0] == marker as u8),
                "receive event {marker} was lost after the bounded window"
            );
        }
    }

    #[test]
    fn control_completion_precedes_a_persistent_card_interrupt_scan() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Ready;
        device.io.receive.active = true;
        device.data.events.push_back(AicEvent::ControlComplete);

        assert!(matches!(
            device.drive_ready(MonotonicTime::from_nanos(0)),
            AicAction::Event(AicEvent::ControlComplete)
        ));
        assert!(device.io.receive.active);
    }

    #[test]
    fn transmit_completion_precedes_a_receive_backlog() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Ready;
        for _ in 0..RX_CAPACITY {
            device.data.events.push_back(AicEvent::Receive(vec![0]));
        }
        device
            .data
            .events
            .push_back(AicEvent::TransmitComplete(TxToken::new(1)));

        assert!(matches!(
            device.drive_ready(MonotonicTime::default()),
            AicAction::Event(AicEvent::TransmitComplete(token)) if token == TxToken::new(1)
        ));
    }

    #[test]
    fn management_frames_are_not_exposed_as_ethernet_events() {
        let management = vec![
            0x80, 0x00, 0, 0, 0, 1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6, 0, 7, 0, 8, 0, 9, 0, 10,
        ];
        assert_eq!(decapsulate_data_frames(&management, 0), None);
    }

    #[test]
    fn qos_data_mpdu_is_decapsulated_to_ethernet() {
        let frame = data_mpdu(0x0288, &[1, 2, 3]);
        let [ethernet] = decapsulate_data_frames(&frame, 0)
            .expect("valid QoS data")
            .try_into()
            .expect("one MSDU");
        assert_eq!(&ethernet[..6], &[1, 2, 3, 4, 5, 6]);
        assert_eq!(&ethernet[6..12], &[0x21, 0x22, 0x23, 0x24, 0x25, 0x26]);
        assert_eq!(&ethernet[12..], &[0x08, 0x00, 1, 2, 3]);
    }

    #[test]
    fn qos_amsdu_subframes_are_decapsulated_to_ethernet() {
        let mut frame = data_mpdu(0x0288, &[]);
        frame.truncate(26);
        frame[24] = 0x80;
        frame.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        frame.extend_from_slice(&[0x21, 0x22, 0x23, 0x24, 0x25, 0x26]);
        frame.extend_from_slice(&11u16.to_be_bytes());
        frame.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0, 0x08, 0x00, 9, 8, 7]);
        frame.extend_from_slice(&[0; 3]);
        frame.extend_from_slice(&[6, 5, 4, 3, 2, 1]);
        frame.extend_from_slice(&[0x26, 0x25, 0x24, 0x23, 0x22, 0x21]);
        frame.extend_from_slice(&10u16.to_be_bytes());
        frame.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0, 0x86, 0xdd, 6, 5]);

        let ethernet = decapsulate_data_frames(&frame, 0).expect("valid A-MSDU subframes");
        assert_eq!(
            ethernet[0],
            [
                1, 2, 3, 4, 5, 6, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x08, 0x00, 9, 8, 7
            ]
        );
        assert_eq!(
            ethernet[1],
            [
                6, 5, 4, 3, 2, 1, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21, 0x86, 0xdd, 6, 5
            ]
        );
    }

    #[test]
    fn encrypted_mpdu_skips_the_ccmp_header_before_llc() {
        let mut frame = data_mpdu(0x0208, &[9, 8]);
        let llc = 24;
        frame.splice(llc..llc, [0, 1, 2, 3, 4, 5, 6, 7]);
        let [ethernet] = decapsulate_data_frames(&frame, 3)
            .expect("valid CCMP data")
            .try_into()
            .expect("one MSDU");
        assert_eq!(&ethernet[12..], &[0x08, 0x00, 9, 8]);
    }

    #[test]
    fn single_function_profile_is_probed_once_per_card_interrupt() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.request_receive_scan();

        let Some(AicAction::SubmitSdio(count)) = device.drive_receive_scan() else {
            panic!("expected the shared command/data function count")
        };
        assert!(matches!(
            count.kind,
            SdioRequestKind::ReadByte { function, .. } if function.get() == 1
        ));
        device.io.pending = None;
        device
            .consume_receive_count(RxPath::Command, SdioResponse::Byte(0))
            .unwrap();

        assert!(device.drive_receive_scan().is_none());
        assert!(!device.io.receive.active);
    }

    #[test]
    fn dc_byte_mode_interrupt_reads_the_length_register_before_the_fifo() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();

        device
            .consume_receive_count(RxPath::Command, SdioResponse::Byte(64))
            .unwrap();

        assert!(matches!(
            device.io.next,
            Some((
                IoPurpose::ReceiveByteLength(RxPath::Command),
            SdioRequestKind::ReadByte { function, address },
        )) if function.get() == 2 && address.get() == 0x02
        ));
    }

    #[test]
    fn dc_transmit_starts_without_reading_data_flow_credits() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Ready;
        device.data.link.install_mac([2, 0, 0, 0, 0, 1]).unwrap();
        device.data.link.install_interface(0).unwrap();
        device
            .data
            .link
            .install_peer(0, 0, [2, 1, 2, 3, 4, 5])
            .unwrap();
        device
            .data
            .tx
            .enqueue(TxToken::new(1), vec![0; 60])
            .unwrap();

        let AicAction::SubmitSdio(request) = device.drive_ready(MonotonicTime::default()) else {
            panic!("expected a direct DC data write")
        };
        assert!(
            matches!(request.kind, SdioRequestKind::Write { function, .. } if function.get() == 1)
        );
    }
}
