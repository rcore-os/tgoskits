use alloc::{vec, vec::Vec};
use core::time::Duration;

use super::*;
use crate::{
    lmac::{
        SM_CONNECT_IND, SM_DISCONNECT_IND, parse_connect_indication, parse_disconnect_indication,
    },
    protocol::{BLOCK_SIZE, TxConfirmation, ethernet_tx_frame},
    registers::ReceiveLength,
    rx::{ParsedFrame, parse_fifo},
};

// The firmware drains one packet buffer per air frame, so a retry no longer
// than a frame's air time finds fresh capacity instead of paying another
// command round trip for an unchanged register.
const IO_RETRY: Duration = Duration::from_micros(200);
// Cap the useful-credit threshold so a large aggregation policy does not add
// disproportionate latency while waiting for a larger batch.
const DATA_TX_MAX_WAIT_CREDITS: u8 = 8;
// How long one thin-pool wait lasts, and how many waits one write may take
// before it goes out with whatever the pool holds.
const DATA_TX_CREDIT_WAIT: Duration = Duration::from_micros(300);
const DATA_TX_CREDIT_WAIT_BUDGET: u32 = 8;
// The firmware reports packet buffers, not SDIO blocks. Keep two buffers
// available for commands, as in the vendor DATA_FLOW_CTRL_THRESH contract.
const DATA_TX_RESERVED_CREDITS: u8 = 2;
const INTERNAL_TX_CAPACITY: usize = 2;
const INTERNAL_TX_BYTE_CAPACITY: usize = 8 * 1024;
const ETHERTYPE_EAPOL: [u8; 2] = [0x88, 0x8e];

impl AicDevice {
    pub(super) fn drive_ready(&mut self, now: MonotonicTime) -> AicAction {
        // The drive path runs, so the next write completion may continue the
        // transmit pipeline once more.
        self.io.chain_used = false;
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
        if let Some(deadline) = self
            .data
            .active_tx
            .as_ref()
            .and_then(|active| active.retry_at)
        {
            if now < deadline {
                return AicAction::WaitForInterruptUntil(deadline);
            }
            if let Some(active) = self.data.active_tx.as_mut() {
                active.retry_at = None;
            }
        }
        if let Some(active) = self.data.active_tx.as_ref() {
            let (purpose, kind) = self.transmit_operation(active);
            return self.emit(purpose, kind);
        }
        AicAction::WaitForInterrupt
    }

    fn take_priority_event(&mut self) -> Option<AicEvent> {
        let index = self
            .data
            .events
            .iter()
            .position(|event| !matches!(event, AicEvent::Receive(_)))?;
        self.data.remove_event(index)
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
        self.arm_receive_scan();
    }

    fn arm_receive_scan(&mut self) {
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
            ReceiveLength::Empty => self.advance_receive_path(),
            ReceiveLength::OtherInterrupt => {
                // The vendor D80 IRQ handler acknowledges the dev-to-host soft
                // IRQ here: read the interrupt-pending register, clear bit 0,
                // and write it back. Without the acknowledgement the pending
                // bit keeps CARD_INT asserted, which both starves the owner
                // loop and leaves the firmware waiting for its interrupt to
                // be consumed.
                self.io.next = Some((
                    IoPurpose::ReceiveOtherAck(path),
                    read_byte(
                        self.receive_function(path),
                        self.registers()
                            .sleep_status
                            .expect("v3 interrupt status implies a sleep-status register"),
                    ),
                ));
            }
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

    pub(super) fn consume_receive_other_ack(
        &mut self,
        path: RxPath,
        response: SdioResponse,
    ) -> Result<(), AicError> {
        let pending = expect_byte(response)?;
        self.io.next = Some((
            IoPurpose::ReceiveOtherClear(path),
            write_byte(
                self.receive_function(path),
                self.registers()
                    .sleep_status
                    .expect("v3 interrupt status implies a sleep-status register"),
                pending & !1,
            ),
        ));
        Ok(())
    }

    pub(super) fn consume_receive_other_clear(
        &mut self,
        path: RxPath,
        response: SdioResponse,
    ) -> Result<(), AicError> {
        // The write is issued with read-after-write; the read-back byte is the
        // register's pre-write value and is not compared.
        let _ = expect_byte(response)?;
        // The vendor D80 handler re-reads the interrupt status after the soft
        // IRQ acknowledgement; stay on the same path until it reads empty.
        self.io.next = Some((
            IoPurpose::ReceiveCount(path),
            read_byte(self.receive_function(path), self.registers().block_count),
        ));
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
        let frames =
            parse_fifo(&receive_data, self.mailbox_confirmation_id()).map_err(|error| {
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
                        } else {
                            self.data.push_event(AicEvent::Receive(frame))?;
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
                    // Firmware can leave an asynchronous association result in
                    // the FIFO across host restart. Startup has no connection
                    // transaction: its owner is handed to the network runtime
                    // before a new Connect request can be submitted. Do not
                    // interpret the old status or install its peer identity.
                    if self.lifecycle.state == AicState::Starting {
                        log::debug!(
                            "[wifi] discarded pre-connection association indication during startup"
                        );
                        continue;
                    }
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
                    self.data.clear_internal_tx();
                    let resetting = self.lifecycle.control.as_ref().is_some_and(|control| {
                        matches!(&control.operation,
                            super::control::ControlOperation::Connect(connect)
                                if connect.phase == super::control::ConnectPhase::Resetting)
                    });
                    if !resetting && self.lifecycle.control.take().is_some() {
                        self.data
                            .push_event(AicEvent::ControlFailed(AicError::Disconnected {
                                reason_code: indication.reason_code,
                            }))?;
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

    /// The transaction that carries the active write: the write itself when a
    /// cached credit still grants it, otherwise the flow-control read whose own
    /// completion continues into the write.
    fn transmit_operation(&self, active: &super::owner::ActiveTx) -> (IoPurpose, SdioRequestKind) {
        if self.tx_credit_available() {
            (
                IoPurpose::TransmitData,
                write_fifo(
                    self.data_function(),
                    self.registers().write_fifo,
                    active.wire_bytes(),
                ),
            )
        } else {
            (
                IoPurpose::TransmitFlow,
                read_byte(self.data_function(), self.registers().flow_control),
            )
        }
    }

    /// Whether the cached firmware credit still grants a data write.
    fn tx_credit_available(&self) -> bool {
        self.data
            .tx_credits
            .is_some_and(|credits| credits > DATA_TX_RESERVED_CREDITS)
    }

    /// Accounts for the packet buffer a completed write handed to the firmware.
    ///
    /// A reading that reaches the command reserve is dropped so the next
    /// transmit re-reads the register instead of writing on a spent quota.
    fn spend_tx_credit(&mut self) {
        self.data.tx_credits = self.data.tx_credits.and_then(|credits| {
            let remaining = credits.saturating_sub(1);
            (remaining > DATA_TX_RESERVED_CREDITS).then_some(remaining)
        });
    }

    /// Drops the cached credit because firmware work outside this state machine
    /// may have taken buffers from the pool the reading described.
    pub(super) fn clear_tx_credits(&mut self) {
        self.data.tx_credits = None;
    }

    pub(super) fn consume_transmit_flow(
        &mut self,
        response: SdioResponse,
        now: MonotonicTime,
    ) -> Result<(), AicError> {
        let credits = self.registers().flow_credits(expect_byte(response)?);
        if self.data.active_tx.is_none() {
            return Err(AicError::CompletionMismatch);
        }
        // Wait only when this user write still has queued frames to append and
        // the selected policy could carry more than one frame. Lifecycle writes
        // and single-packet policies use the available credit immediately.
        let waits = self
            .data
            .active_tx
            .as_ref()
            .map_or(0, |active| active.credit_waits);
        let has_queued_frame = !self.data.tx.is_empty();
        let can_grow = self.user_write_in_flight()
            && self.tx_aggregation.packets > 1
            && has_queued_frame
            && self.active_write_below_byte_target();
        let wait_threshold = self.aggregation_credit_threshold();
        let thin = can_grow && credits > DATA_TX_RESERVED_CREDITS && credits < wait_threshold;
        if thin && waits < DATA_TX_CREDIT_WAIT_BUDGET {
            if let Some(active) = self.data.active_tx.as_mut() {
                active.credit_waits = waits.saturating_add(1);
                active.retry_at = Some(now.after(DATA_TX_CREDIT_WAIT));
            }
            self.data.tx_credits = None;
            return Ok(());
        }
        // One written packet consumes one reported buffer, so the reading
        // authorises the next writes until the reserve boundary. Reserve-only
        // readings are not cached because they cannot authorize a data write.
        self.data.tx_credits = (credits > DATA_TX_RESERVED_CREDITS).then_some(credits);
        if credits <= DATA_TX_RESERVED_CREDITS {
            let active = self
                .data
                .active_tx
                .as_mut()
                .ok_or(AicError::CompletionMismatch)?;
            active.retry_at = Some(now.after(IO_RETRY));
            return Ok(());
        }
        // The reading is the first moment the number of available firmware
        // buffers is known, so the burst is grown here as well.  Internal
        // lifecycle writes stay single-packet: their completions have no user
        // token to release.
        if self.user_write_in_flight() {
            let limit = self.aggregate_limit();
            self.extend_active_write(limit);
        }
        let active = self
            .data
            .active_tx
            .as_ref()
            .ok_or(AicError::CompletionMismatch)?;
        let frame = active.wire_bytes();
        self.io.next = Some((
            IoPurpose::TransmitData,
            write_fifo(self.data_function(), self.registers().write_fifo, frame),
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
        // Every packet the write carried consumed one firmware buffer.
        for _ in 0..active.packets() {
            self.spend_tx_credit();
        }
        match active.completion {
            super::owner::TxCompletion::User(token) => {
                let mut tokens = Vec::with_capacity(active.packets());
                tokens.push(token);
                tokens.extend(active.extra_tokens);
                self.complete_write_tokens(tokens);
            }
            super::owner::TxCompletion::Internal(kind) => {
                // A lifecycle write carries no user packet, so it accumulates
                // no extra token.  Releasing them here anyway keeps a buffer
                // that ever got appended to one from staying owned forever.
                self.complete_write_tokens(active.extra_tokens);
                if kind == super::owner::InternalTxKind::M4 {
                    let (station_index, _) = self.data.link.peer().ok_or(AicError::WpaProtocol)?;
                    self.lifecycle
                        .control
                        .as_mut()
                        .ok_or(AicError::CompletionMismatch)?
                        .accept_m4_transmit(station_index)?;
                }
            }
        }
        Ok(())
    }

    /// Takes the write in flight, returning the tokens of the packets it
    /// carried in the order they were handed over.
    pub(super) fn take_active_write_tokens(&mut self) -> Vec<TxToken> {
        let Some(active) = self.data.active_tx.take() else {
            return Vec::new();
        };
        let mut tokens = active.extra_tokens;
        if let super::owner::TxCompletion::User(token) = active.completion {
            tokens.insert(0, token);
        }
        tokens
    }

    /// Whether the write in flight carries user packets rather than a lifecycle
    /// frame.
    fn user_write_in_flight(&self) -> bool {
        matches!(
            self.data
                .active_tx
                .as_ref()
                .map(|active| &active.completion),
            Some(super::owner::TxCompletion::User(_))
        )
    }

    /// Publishes the packets one transmit write completed.  A write that
    /// carried a single packet keeps the per-packet event; a burst is published
    /// as one event, so a saturated transmitter cannot crowd the receive frames
    /// out of the shared event queue with one entry per packet.  With no room
    /// the tokens wait instead of displacing anything: a completion only ever
    /// returns a buffer, and the next drive pass has room again once the queue
    /// drains.
    pub(super) fn complete_write_tokens(&mut self, tokens: Vec<TxToken>) {
        if tokens.is_empty() {
            return;
        }
        if self.data.event_room() == 0 {
            self.data.pending_completions.extend(tokens);
            return;
        }
        let event = if tokens.len() == 1 {
            AicEvent::TransmitComplete(tokens[0])
        } else {
            AicEvent::TransmitAggregateComplete(tokens)
        };
        let _ = self.data.push_event(event);
    }

    /// Queues the next transmit write straight from the previous write's
    /// completion.
    ///
    /// A bulk transfer spends its packet period waiting for the runtime to hand
    /// the owner its next advance: the write completes, its completion event
    /// goes up, and the next write is only formed when the owner is driven
    /// again.  When a frame is already queued and the cached credit still
    /// grants a write, the next write is queued here instead, so the owner
    /// emits it on the next advance ahead of the receive scan.
    ///
    /// The ordering guarantees are untouched: the write is still the only
    /// transaction in flight, its completion still releases the tokens, a
    /// queued operation keeps its turn, and the receive scan is never
    /// postponed across advances — it simply runs one write period later.
    pub(super) fn continue_transmit_pipeline(&mut self, now: MonotonicTime) {
        if self.lifecycle.state != AicState::Ready {
            return;
        }
        if self.io.next.is_some() {
            return;
        }
        // One continued write per drive pass: with a saturated transmitter the
        // queue is never empty, so an unbounded continuation would keep the
        // receive side from draining the firmware buffers at all.  The drive
        // path clears the bound when it runs, which is what gives the receive
        // scan and the mailbox their turn between two writes.
        if self.io.chain_used {
            return;
        }
        self.prepare_next_transmit();
        let Some(active) = self.data.active_tx.as_ref() else {
            return;
        };
        if active.retry_at.is_some_and(|deadline| now < deadline) {
            return;
        }
        let (purpose, kind) = self.transmit_operation(active);
        self.io.next = Some((purpose, kind));
        self.io.chain_used = true;
    }

    /// Drops the transmit request a continuation queued but never submitted.
    ///
    /// The frames it would have carried are the ones `active_tx` owns, so only
    /// the queued operation is dropped here: releasing their tokens happens in
    /// one place, on the write that holds them.  Receive continuations queued
    /// in the same slot are left alone.
    pub(super) fn discard_pending_transmit(&mut self) {
        if matches!(
            self.io.next,
            Some((IoPurpose::TransmitData | IoPurpose::TransmitFlow, _))
        ) {
            self.io.next = None;
            self.io.chain_used = false;
        }
    }

    fn prepare_next_transmit(&mut self) {
        if self.data.active_tx.is_some() {
            return;
        }
        let Some((interface_index, station_index)) = self.data.link.tx_indices() else {
            return;
        };
        if let Some(internal) = self.data.pop_internal_tx() {
            let Ok((wire_frame, stream_len)) = ethernet_tx_frame(
                &internal.ethernet_frame,
                interface_index,
                station_index,
                self.transport_uses_header_crc(),
                TxConfirmation::Firmware,
            ) else {
                return;
            };
            self.data.active_tx = Some(ActiveTx::new(
                super::owner::TxCompletion::Internal(internal.kind),
                wire_frame,
                stream_len,
            ));
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
            Ok(wire) => {
                self.data.active_tx = Some(ActiveTx::new(
                    super::owner::TxCompletion::User(wire.token),
                    wire.bytes,
                    wire.stream_len,
                ));
                let limit = self.aggregate_limit();
                self.extend_active_write(limit);
            }
            Err(token) => self.complete_write_tokens(vec![token]),
        }
    }

    /// Packets the next write may carry, bounded by policy and cached credit.
    fn aggregate_limit(&self) -> usize {
        self.data
            .tx_credits
            .map(|credits| self.aggregate_limit_for(credits))
            .unwrap_or(1)
    }

    fn aggregate_limit_for(&self, credits: u8) -> usize {
        usize::from(credits.saturating_sub(DATA_TX_RESERVED_CREDITS))
            .min(self.tx_aggregation.packets)
    }

    fn active_write_below_byte_target(&self) -> bool {
        self.data
            .active_tx
            .as_ref()
            .is_some_and(|active| active.stream_len < self.tx_aggregation.bytes)
    }

    /// Credit level below which a write waits for a larger pool: the frames the
    /// policy could still append, capped so a large policy does not add its
    /// whole width to every thin reading.
    fn aggregation_credit_threshold(&self) -> u8 {
        let useful_frames = self.tx_aggregation.packets.min(usize::from(
            DATA_TX_MAX_WAIT_CREDITS.saturating_sub(DATA_TX_RESERVED_CREDITS),
        ));
        DATA_TX_RESERVED_CREDITS
            .saturating_add(u8::try_from(useful_frames).unwrap_or(DATA_TX_MAX_WAIT_CREDITS))
    }

    /// Grows the pending write up to `limit` packets by appending frames that
    /// are already queued.  Each frame keeps its own header inside the write,
    /// so the firmware walks them as a stream.  Growth is opportunistic: it
    /// takes what is queued at this moment and flushes it, rather than holding
    /// a formed write back for frames that have not arrived.
    fn extend_active_write(&mut self, limit: usize) {
        let Some((interface_index, station_index)) = self.data.link.tx_indices() else {
            return;
        };
        let crc = self.transport_uses_header_crc();
        loop {
            let packets = self
                .data
                .active_tx
                .as_ref()
                .map_or(0, super::owner::ActiveTx::packets);
            let bytes = self
                .data
                .active_tx
                .as_ref()
                .map_or(0, |active| active.stream_len);
            if packets == 0 || packets >= limit || bytes >= self.tx_aggregation.bytes {
                return;
            }
            let Some(next) = self
                .data
                .tx
                .take_wire_frame(interface_index, station_index, crc)
            else {
                return;
            };
            match next {
                Ok(wire) => {
                    if let Some(active) = self.data.active_tx.as_mut() {
                        active.append_frame(&wire.bytes, wire.stream_len);
                        active.extra_tokens.push(wire.token);
                    }
                }
                Err(token) => self.complete_write_tokens(vec![token]),
            }
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
        let ethernet_length = 14usize
            .checked_add(eapol.len())
            .ok_or(AicError::TxQueueFull)?;
        if self.data.internal_tx.len() >= INTERNAL_TX_CAPACITY
            || self
                .data
                .internal_tx_bytes
                .checked_add(ethernet_length)
                .is_none_or(|bytes| bytes > INTERNAL_TX_BYTE_CAPACITY)
        {
            return Err(AicError::TxQueueFull);
        }
        let local_mac = self
            .data
            .link
            .mac_address()
            .ok_or(AicError::InvalidMacAddress)?;
        let (_, bssid) = self.data.link.peer().ok_or(AicError::WpaProtocol)?;
        let mut ethernet = Vec::with_capacity(ethernet_length);
        ethernet.extend_from_slice(&bssid);
        ethernet.extend_from_slice(&local_mac);
        ethernet.extend_from_slice(&ETHERTYPE_EAPOL);
        ethernet.extend_from_slice(&eapol);
        self.data.internal_tx_bytes += ethernet.len();
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
        // A-MSDU is only expected from a peer that saw the capability this
        // driver advertises, so the frame is taken apart into the Ethernet
        // frames it aggregates.
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
        TxAggregation,
        common::{ChipVariant, SDIO_TYPE_CFG_CMD_RSP, SDIO_TYPE_CFG_PRINT, SDIO_TYPE_DATA},
        rx::{RX_BYTE_CAPACITY, RX_CAPACITY},
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
    fn startup_ignores_unowned_connect_results_and_keeps_mailbox_confirmation() {
        for status in [1u16, 0] {
            let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
            device.start(MonotonicTime::default()).unwrap();
            device.lifecycle.mailbox =
                Some(super::super::mailbox::MailboxState::confirmation_for_test(
                    MonotonicTime::from_nanos(5_000_000_000),
                ));
            let mut payload = vec![0; 11];
            payload[..2].copy_from_slice(&status.to_le_bytes());
            let mut fifo = indication_fifo(SM_CONNECT_IND, &payload);
            fifo.extend(indication_fifo(2, &[]));
            device.io.pending = Some(PendingIo {
                id: 7,
                purpose: IoPurpose::ReceiveData(RxPath::Command),
            });
            let action = device.advance(AicInput {
                now: MonotonicTime::default(),
                event: Some(AicInputEvent::Sdio(SdioCompletion {
                    request_id: 7,
                    result: Ok(SdioResponse::Data(fifo)),
                })),
            });
            assert!(
                matches!(action, AicAction::SubmitSdio(_)),
                "unowned connection result stopped startup: {action:?}"
            );
            assert_eq!(device.state(), AicState::Starting);
            assert!(device.data.link.peer().is_none());
            assert_eq!(
                device.accept_mailbox_confirmation(2, Vec::new()),
                Err(AicError::CompletionMismatch)
            );
        }
    }

    #[test]
    fn active_connect_rejection_is_not_discarded_as_a_startup_indication() {
        let mut device = ready_transmitter(ChipVariant::Aic8800DC, 60);
        let mut control = super::super::control::build(
            ControlRequest::Connect {
                ssid: b"network".to_vec(),
                pmk: None,
                entropy: None,
            },
            [2, 0, 0, 0, 0, 1],
            Some(0),
        )
        .unwrap();
        if let super::super::control::ControlOperation::Connect(connect) = &mut control.operation {
            connect.phase = super::super::control::ConnectPhase::AwaitIndication;
        }
        control.commands.clear();
        device.lifecycle.control = Some(control);
        let mut payload = vec![0; 11];
        payload[0] = 1;
        assert_eq!(
            device.consume_receive_data(
                RxPath::Command,
                SdioResponse::Data(indication_fifo(SM_CONNECT_IND, &payload)),
            ),
            Err(AicError::FirmwareRejected {
                message_id: SM_CONNECT_IND,
                status: 1
            })
        );
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
    fn mailbox_confirmation_survives_control_budget_exhaustion() {
        const PRINT_PACKET_LENGTH: usize = 8;
        const PRINT_AGGREGATE_LENGTH: usize = 4 + PRINT_PACKET_LENGTH;
        const RESPONSE_PACKET_LENGTH: usize = 12;
        const RESPONSE_AGGREGATE_LENGTH: usize = 4 + RESPONSE_PACKET_LENGTH;
        const EXPECTED_MESSAGE_ID: u16 = 2;

        let response_offset = crate::rx::CONTROL_RX_CAPACITY * PRINT_AGGREGATE_LENGTH;
        let mut fifo = vec![0; response_offset + RESPONSE_AGGREGATE_LENGTH];
        for index in 0..crate::rx::CONTROL_RX_CAPACITY {
            let offset = index * PRINT_AGGREGATE_LENGTH;
            fifo[offset..offset + 2].copy_from_slice(&(PRINT_PACKET_LENGTH as u16).to_le_bytes());
            fifo[offset + 2] = SDIO_TYPE_CFG_PRINT;
        }
        fifo[response_offset..response_offset + 2]
            .copy_from_slice(&(RESPONSE_PACKET_LENGTH as u16).to_le_bytes());
        fifo[response_offset + 2] = SDIO_TYPE_CFG_CMD_RSP;
        fifo[response_offset + 4..response_offset + 6]
            .copy_from_slice(&EXPECTED_MESSAGE_ID.to_le_bytes());

        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.mailbox = Some(MailboxState::confirmation_for_test(
            MonotonicTime::from_nanos(10),
        ));
        device.io.receive.active = true;

        device
            .consume_receive_data(RxPath::Command, SdioResponse::Data(fifo))
            .unwrap();

        assert_eq!(device.mailbox_confirmation_id(), None);
        assert!(!device.mailbox_waiting_for_receive());
    }

    #[test]
    fn receive_events_do_not_stall_after_the_first_bounded_window() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Ready;

        for marker in 0..=RX_CAPACITY {
            device
                .consume_receive_data(RxPath::Command, SdioResponse::Data(data_fifo(marker as u8)))
                .unwrap();
            let event = device.data.pop_event();
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
        device.data.push_event(AicEvent::ControlComplete).unwrap();

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
            device.data.push_event(AicEvent::Receive(vec![0])).unwrap();
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
    fn receive_event_queue_obeys_item_and_byte_limits() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        for _ in 0..RX_CAPACITY {
            device
                .data
                .push_event(AicEvent::Receive(vec![0; 2048]))
                .unwrap();
        }
        device
            .data
            .push_event(AicEvent::Receive(vec![0; 2048]))
            .unwrap();

        assert_eq!(device.data.events.len(), RX_CAPACITY);
        assert_eq!(device.data.event_bytes, RX_BYTE_CAPACITY);

        device.data.push_event(AicEvent::ControlComplete).unwrap();
        assert_eq!(device.data.events.len(), RX_CAPACITY);
        assert!(
            device
                .data
                .events
                .iter()
                .any(|event| matches!(event, AicEvent::ControlComplete))
        );
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

    fn ready_transmitter(chip: ChipVariant, frame_len: usize) -> AicDevice {
        let mut device = AicDevice::new(chip).unwrap();
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
            .enqueue(TxToken::new(1), vec![0; frame_len])
            .unwrap();
        device
    }

    #[test]
    fn transmit_backoff_services_card_irq_without_retrying_credits_early() {
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 1414);
        let now = MonotonicTime::default();
        let AicAction::SubmitSdio(flow) = device.advance(AicInput::tick(now)) else {
            panic!("expected credit read")
        };
        let wait = device.advance(complete(&flow, SdioResponse::Byte(0), now));
        let deadline = now.after(IO_RETRY);
        assert_eq!(wait, AicAction::WaitForInterruptUntil(deadline));
        let AicAction::SubmitSdio(rx) = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::Irq(IrqSnapshot {
                sequence: 1,
                card_interrupt: true,
                transfer_complete: false,
                error: None,
            })),
        }) else {
            panic!("RX must run during TX backoff")
        };
        assert!(matches!(rx.kind, SdioRequestKind::ReadByte { address, .. }
            if address.get() == device.registers().block_count));
        assert_eq!(
            device.advance(complete(&rx, SdioResponse::Byte(0), now)),
            AicAction::WaitForInterruptUntil(deadline)
        );
        let AicAction::SubmitSdio(flow) = device.advance(AicInput::tick(deadline)) else {
            panic!("credit retry must resume at its original deadline")
        };
        let AicAction::SubmitSdio(write) =
            device.advance(complete(&flow, SdioResponse::Byte(128), deadline))
        else {
            panic!("D80 full-byte credit must permit transmission")
        };
        assert!(matches!(write.kind, SdioRequestKind::Write { .. }));
        assert_eq!(
            device.advance(complete(&write, SdioResponse::Unit, deadline)),
            AicAction::Event(AicEvent::TransmitComplete(TxToken::new(1)))
        );
    }

    #[test]
    fn dc_data_tx_checks_firmware_credits_before_writing() {
        let mut device = ready_transmitter(ChipVariant::Aic8800DC, 60);
        let AicAction::SubmitSdio(request) = device.advance(AicInput {
            now: MonotonicTime::default(),
            event: None,
        }) else {
            panic!("expected a DC data credit read")
        };
        assert!(matches!(request.kind,
            SdioRequestKind::ReadByte { function, address }
            if function.get() == 1 && address.get() == 0x0a));
    }

    #[test]
    fn thin_credit_wait_is_bounded_and_eventually_writes_the_available_batch() {
        let now = MonotonicTime::default();
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        device.set_tx_aggregation(aggregate(4)).unwrap();
        device
            .data
            .tx
            .enqueue(TxToken::new(2), vec![0; 60])
            .unwrap();

        let mut current = now;
        let mut action = device.advance(AicInput::tick(current));
        for _ in 0..DATA_TX_CREDIT_WAIT_BUDGET {
            let flow = submit(&action);
            assert!(matches!(flow.kind, SdioRequestKind::ReadByte { .. }));
            action = device.advance(complete(&flow, SdioResponse::Byte(3), current));
            let AicAction::WaitForInterruptUntil(deadline) = action else {
                panic!("thin but useful pool must wait within the retry budget")
            };
            assert_eq!(deadline, current.after(DATA_TX_CREDIT_WAIT));
            current = deadline;
            action = device.advance(AicInput::tick(current));
        }

        let flow = submit(&action);
        action = device.advance(complete(&flow, SdioResponse::Byte(3), current));
        assert!(
            matches!(
                action,
                AicAction::SubmitSdio(SdioRequest {
                    kind: SdioRequestKind::Write { .. },
                    ..
                })
            ),
            "the bounded wait must write the available credit"
        );
    }

    #[test]
    fn reserve_credit_read_is_not_cached() {
        let now = MonotonicTime::default();
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        let flow = submit(&device.advance(AicInput::tick(now)));
        let retry = device.advance(complete(&flow, SdioResponse::Byte(2), now));

        assert!(matches!(retry, AicAction::WaitForInterruptUntil(_)));
        assert_eq!(device.data.tx_credits, None);
    }

    #[test]
    fn a_pool_that_cannot_back_the_batch_does_not_buy_a_smaller_write() {
        // One full-sized packet consumes one firmware buffer, not three
        // 512-byte SDIO blocks. Two buffers stay reserved for commands, and a
        // write pays its fixed cost whatever it carries, so while the reported
        // pool cannot back the selected batch the queued packets stay queued.
        for chip in [ChipVariant::Aic8800D80, ChipVariant::Aic8800DC] {
            let mut device = ready_transmitter(chip, 1414);
            device.set_tx_aggregation(aggregate(4)).unwrap();
            device
                .data
                .tx
                .enqueue(TxToken::new(2), vec![0; 1414])
                .unwrap();

            let mut now = MonotonicTime::default();
            let mut action = device.advance(AicInput::tick(now));
            for credits in [0, 1, DATA_TX_RESERVED_CREDITS, DATA_TX_RESERVED_CREDITS + 3] {
                let flow = submit(&action);
                assert!(matches!(flow.kind, SdioRequestKind::ReadByte { .. }));
                action = device.advance(complete(&flow, SdioResponse::Byte(credits), now));
                let AicAction::WaitForInterruptUntil(deadline) = action else {
                    panic!("a pool of {credits} buffers must not buy a batch write")
                };
                assert_eq!(device.data.active_tx.as_ref().unwrap().packets(), 1);
                assert!(device.data.events.is_empty());
                now = deadline;
                action = device.advance(AicInput::tick(now));
            }

            let flow = submit(&action);
            let write = submit(&device.advance(complete(
                &flow,
                SdioResponse::Byte(DATA_TX_RESERVED_CREDITS + 4),
                now,
            )));
            assert_eq!(
                written_frames(&AicAction::SubmitSdio(write.clone())),
                vec![1428, 1428],
                "the reading that can back the batch writes every queued packet"
            );
            assert!(device.data.tx.is_empty());

            assert_eq!(
                device.advance(complete(&write, SdioResponse::Unit, now)),
                AicAction::Event(AicEvent::TransmitAggregateComplete(vec![
                    TxToken::new(1),
                    TxToken::new(2)
                ])),
                "the batch completes every packet it carried"
            );
            assert_eq!(
                device.data.tx_credits,
                Some(DATA_TX_RESERVED_CREDITS + 2),
                "one firmware buffer per packet the write carried"
            );
        }
    }

    #[test]
    fn one_credit_read_serves_a_packet_burst() {
        // A reading reports packet buffers, so it authorises as many writes:
        // consecutive packets must not pay for another command round trip.
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        let now = MonotonicTime::default();
        let AicAction::SubmitSdio(flow) = device.advance(AicInput::tick(now)) else {
            panic!("expected the first firmware credit read")
        };
        let mut action = device.advance(complete(&flow, SdioResponse::Byte(32), now));
        for packet in 2..=5u8 {
            let AicAction::SubmitSdio(write) = action else {
                panic!("a cached credit must admit packet {packet} without a credit read")
            };
            assert!(matches!(write.kind, SdioRequestKind::Write { .. }));
            device
                .data
                .tx
                .enqueue(TxToken::new(u64::from(packet)), vec![0; 60])
                .unwrap();
            assert!(matches!(
                device.advance(complete(&write, SdioResponse::Unit, now)),
                AicAction::Event(AicEvent::TransmitComplete(token))
                    if token == TxToken::new(u64::from(packet - 1))
            ));
            assert_eq!(device.data.tx_credits, Some(33 - packet));
            action = device.advance(AicInput::tick(now));
        }
        assert!(
            matches!(
                action,
                AicAction::SubmitSdio(SdioRequest {
                    kind: SdioRequestKind::Write { .. },
                    ..
                })
            ),
            "the burst continues while the cached credit lasts"
        );
    }

    #[test]
    fn one_write_carries_a_packet_batch_and_completes_every_token() {
        // A transmit write is a stream of self-delimiting frames, so several
        // queued packets can share one CMD53.  Every packet still consumes one
        // firmware buffer and completes its own token.
        let now = MonotonicTime::default();
        let mut single = ready_transmitter(ChipVariant::Aic8800D80, 60);
        single
            .set_tx_aggregation(TxAggregation::new(1, TxAggregation::DEFAULT_BYTES))
            .unwrap();
        single.data.tx_credits = Some(16);
        assert_eq!(
            written_frames(&single.advance(AicInput::tick(now))),
            vec![74],
            "a single-packet write ends its stream after its own frame"
        );

        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        device.set_tx_aggregation(aggregate(4)).unwrap();
        device.data.tx_credits = Some(16);
        let batched = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::TxBatch(vec![
                (TxToken::new(2), vec![0; 64]),
                (TxToken::new(3), vec![0; 68]),
                (TxToken::new(4), vec![0; 72]),
            ])),
        });
        let AicAction::SubmitSdio(write) = batched else {
            panic!("a cached credit must admit the whole batch")
        };
        assert_eq!(
            written_frames(&AicAction::SubmitSdio(write.clone())),
            vec![74, 78, 82, 86],
            "the firmware's walk must reach every packet the write carries"
        );
        assert!(
            write_frame_len(&AicAction::SubmitSdio(write.clone())).is_multiple_of(BLOCK_SIZE),
            "the write as a whole stays block aligned"
        );
        assert_eq!(
            device.data.tx_credits,
            Some(16),
            "credits are spent when the write completes, not when it is emitted"
        );

        let completed = match device.advance(complete(&write, SdioResponse::Unit, now)) {
            AicAction::Event(AicEvent::TransmitAggregateComplete(tokens)) => tokens,
            other => panic!("unexpected action for an aggregated write: {other:?}"),
        };
        assert_eq!(
            completed,
            vec![
                TxToken::new(1),
                TxToken::new(2),
                TxToken::new(3),
                TxToken::new(4)
            ],
            "one write publishes its packets as one completion event"
        );
        assert_eq!(
            device.data.tx_credits,
            Some(12),
            "one firmware buffer per packet the write carried"
        );
    }

    #[test]
    fn the_soft_byte_target_allows_one_crossing_frame_then_stops_growth() {
        // The target is checked before appending, so one frame may cross it.
        // That frame remains intact and later frames stay queued.
        let now = MonotonicTime::default();
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        device
            .set_tx_aggregation(TxAggregation::new(32, 200))
            .unwrap();
        device.data.tx_credits = Some(32);

        let write = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::TxBatch(vec![
                (TxToken::new(2), vec![0; 64]),
                (TxToken::new(3), vec![0; 68]),
                (TxToken::new(4), vec![0; 72]),
            ])),
        });

        assert_eq!(written_frames(&write), vec![74, 78, 82]);
        assert_eq!(
            device.data.active_tx.as_ref().unwrap().stream_len,
            252,
            "the final frame may take the stream past the 200-byte target"
        );
        assert_eq!(
            device.data.tx.len(),
            1,
            "the frame that did not fit waits for the next write"
        );
    }

    #[test]
    fn a_full_event_queue_defers_batch_completions_without_displacing_receive_frames() {
        // Completions share the event queue with received frames.  A burst must
        // wait for room instead of evicting a frame, and must be published once
        // the queue drains, without any further stimulus.
        let now = MonotonicTime::default();
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        device.set_tx_aggregation(aggregate(4)).unwrap();
        device.data.tx_credits = Some(16);
        let AicAction::SubmitSdio(write) = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::TxBatch(vec![
                (TxToken::new(2), vec![0; 64]),
                (TxToken::new(3), vec![0; 68]),
                (TxToken::new(4), vec![0; 72]),
            ])),
        }) else {
            panic!("a cached credit must admit the whole batch")
        };
        for _ in 0..RX_CAPACITY {
            device
                .data
                .push_event(AicEvent::Receive(vec![0; 64]))
                .unwrap();
        }

        let first = device.advance(complete(&write, SdioResponse::Unit, now));

        assert!(matches!(first, AicAction::Event(AicEvent::Receive(_))));
        assert_eq!(
            device
                .data
                .pending_completions
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![
                TxToken::new(1),
                TxToken::new(2),
                TxToken::new(3),
                TxToken::new(4)
            ],
            "a full queue holds the write's packets back"
        );
        assert_eq!(
            device.data.events.len(),
            RX_CAPACITY - 1,
            "no received frame is displaced by the completions"
        );

        let mut received = 0;
        let mut completed = Vec::new();
        while !device.data.events.is_empty() || !device.data.pending_completions.is_empty() {
            match device.advance(AicInput::tick(now)) {
                AicAction::Event(AicEvent::Receive(_)) => received += 1,
                AicAction::Event(AicEvent::TransmitComplete(token)) => completed.push(token),
                AicAction::Event(AicEvent::TransmitAggregateComplete(tokens)) => {
                    completed.extend(tokens);
                }
                other => panic!("the queue must drain before anything else: {other:?}"),
            }
        }

        assert_eq!(
            received,
            RX_CAPACITY - 1,
            "every queued frame survives the completion burst"
        );
        assert_eq!(
            completed,
            vec![
                TxToken::new(1),
                TxToken::new(2),
                TxToken::new(3),
                TxToken::new(4)
            ],
            "the held-back completions are published once the queue drains"
        );
    }

    #[test]
    fn a_lifecycle_write_never_carries_user_packets() {
        // A lifecycle write is emitted while user frames are queued.  Its
        // completion carries no user token, so a packet appended to it would
        // never return its buffer.
        let now = MonotonicTime::default();
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        device.set_tx_aggregation(aggregate(4)).unwrap();
        device
            .queue_internal_eapol(super::super::owner::InternalTxKind::M2, vec![0; 95])
            .unwrap();
        // The cache is spent down to the reserve, so the transmit has to read
        // firmware buffers first: the moment a burst is normally grown.
        device.data.tx_credits = Some(DATA_TX_RESERVED_CREDITS);

        let AicAction::SubmitSdio(flow) = device.advance(AicInput::tick(now)) else {
            panic!("a spent credit must be re-read before writing")
        };
        let write = device.advance(complete(&flow, SdioResponse::Byte(16), now));

        assert_eq!(
            written_frames(&write),
            vec![123],
            "the lifecycle write carries its own frame only"
        );
        assert_eq!(
            device.data.tx.len(),
            1,
            "the queued user frame stays queued for a user write"
        );
        assert!(
            device
                .data
                .active_tx
                .as_ref()
                .is_some_and(|active| active.extra_tokens.is_empty()),
            "a lifecycle write owns no user token"
        );
    }

    #[test]
    fn a_batch_that_does_not_fit_releases_the_tokens_it_drops() {
        // The transmit queue is bounded.  A batch that cannot be queued in full
        // must not fail the device, and every packet it drops must still return
        // its buffer.
        let now = MonotonicTime::default();
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        while device.data.tx.len() + 1 < crate::tx::TX_CAPACITY {
            let token = TxToken::new(device.data.tx.len() as u64 + 1);
            device
                .data
                .tx
                .enqueue(token, vec![0; 60])
                .expect("the queue is filled below its capacity");
        }

        let action = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::TxBatch(vec![
                (TxToken::new(200), vec![0; 60]),
                (TxToken::new(201), vec![0; 60]),
                (TxToken::new(202), vec![0; 60]),
            ])),
        });

        assert_eq!(
            device.state(),
            AicState::Ready,
            "a batch larger than the free room must not fail the device"
        );
        assert_eq!(
            action,
            AicAction::Event(AicEvent::TransmitAggregateComplete(vec![
                TxToken::new(201),
                TxToken::new(202)
            ])),
            "the dropped packets are reported complete"
        );
        assert_eq!(device.data.tx.len(), crate::tx::TX_CAPACITY);
    }

    #[test]
    fn a_single_frame_that_does_not_fit_is_reported_complete() {
        // The single-frame form is the same over-capacity case as the batch:
        // the packet returns to the runtime instead of failing the device.
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        while device.data.tx.len() < crate::tx::TX_CAPACITY {
            let token = TxToken::new(device.data.tx.len() as u64 + 1);
            device
                .data
                .tx
                .enqueue(token, vec![0; 60])
                .expect("the queue is filled up to its capacity");
        }

        let action = device.advance(AicInput {
            now: MonotonicTime::default(),
            event: Some(AicInputEvent::Tx {
                token: TxToken::new(200),
                frame: vec![0; 60],
            }),
        });

        assert_eq!(
            device.state(),
            AicState::Ready,
            "a full queue must not fail the device"
        );
        assert_eq!(
            action,
            AicAction::Event(AicEvent::TransmitComplete(TxToken::new(200))),
            "the packet that did not fit is reported complete"
        );
        assert_eq!(device.data.tx.len(), crate::tx::TX_CAPACITY);
    }

    #[test]
    fn cancellation_releases_the_write_in_flight() {
        // Cancelling aborts the transaction in flight.  The aborted write is
        // dropped, so its packets are reported complete instead of being
        // written later.
        let now = MonotonicTime::default();
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        device.data.tx_credits = Some(16);
        let AicAction::SubmitSdio(write) = device.advance(AicInput::tick(now)) else {
            panic!("a cached credit must admit the queued frame")
        };
        let mut control = super::super::control::build(
            ControlRequest::Connect {
                ssid: b"network".to_vec(),
                pmk: None,
                entropy: None,
            },
            [2, 0, 0, 0, 0, 1],
            Some(0),
        )
        .unwrap();
        control.commands.clear();
        device.lifecycle.control = Some(control);

        let AicAction::AbortSdio { request_id } = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::Control(ControlRequest::Cancel)),
        }) else {
            panic!("cancellation must abort the write in flight")
        };
        assert_eq!(request_id, write.id);

        let abort = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::Sdio(SdioCompletion {
                request_id,
                result: Err(SdioFailure::Aborted),
            })),
        });

        assert_eq!(
            abort,
            AicAction::Event(AicEvent::TransmitComplete(TxToken::new(1))),
            "the aborted write returns its packet's buffer"
        );
        assert!(device.data.active_tx.is_none());
        assert!(device.data.tx.is_empty());
    }

    /// Aggregate policy carrying `packets` frames under the default byte bound.
    fn aggregate(packets: usize) -> TxAggregation {
        TxAggregation::new(packets, TxAggregation::DEFAULT_BYTES)
    }

    /// Walks the frames inside a transmit write the way the firmware does: over
    /// each frame's declared length rounded up to the transmit alignment, and
    /// stopping at the first zero length.  Returns the declared length of every
    /// frame the walk reached.
    fn written_frames(action: &AicAction) -> Vec<usize> {
        let AicAction::SubmitSdio(request) = action else {
            panic!("expected a transmit write, got {action:?}")
        };
        let SdioRequestKind::Write { bytes, .. } = &request.kind else {
            panic!("expected a transmit write")
        };
        let mut frames = Vec::new();
        let mut offset = 0;
        while let Some(header) = bytes.get(offset..offset + 2) {
            let declared = usize::from(u16::from_le_bytes([header[0], header[1]])) & 0x0fff;
            if declared == 0 {
                break;
            }
            frames.push(declared);
            offset += (crate::protocol::SDIO_HEADER_SIZE + declared).next_multiple_of(4);
        }
        frames
    }

    fn write_frame_len(action: &AicAction) -> usize {
        let AicAction::SubmitSdio(request) = action else {
            panic!("expected a transmit write, got {action:?}")
        };
        let SdioRequestKind::Write { bytes, .. } = &request.kind else {
            panic!("expected a transmit write")
        };
        bytes.len()
    }

    #[test]
    fn command_traffic_clears_the_cached_tx_credit() {
        // The command mailbox takes buffers from the same firmware pool as data
        // writes, so a credit read before it can no longer be trusted.
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 60);
        let now = MonotonicTime::default();
        device.data.tx_credits = Some(64);
        device.lifecycle.mailbox = Some(MailboxState::confirmation_for_test(
            now.after(Duration::from_millis(10)),
        ));
        device.io.pending = Some(PendingIo {
            id: 4,
            purpose: IoPurpose::MailboxWrite,
        });

        device.advance(AicInput {
            now,
            event: Some(AicInputEvent::Sdio(SdioCompletion {
                request_id: 4,
                result: Ok(SdioResponse::Unit),
            })),
        });
        assert_eq!(
            device.data.tx_credits, None,
            "a mailbox write spends firmware buffers the cached credit counted"
        );
    }

    fn complete(request: &SdioRequest, response: SdioResponse, now: MonotonicTime) -> AicInput {
        AicInput {
            now,
            event: Some(AicInputEvent::Sdio(SdioCompletion {
                request_id: request.id,
                result: Ok(response),
            })),
        }
    }

    /// Whether the next action reads the receive count register, the first
    /// transaction of a receive scan.
    fn starts_receive_scan(action: &AicAction, device: &AicDevice) -> bool {
        let AicAction::SubmitSdio(request) = action else {
            return false;
        };
        matches!(
            request.kind,
            SdioRequestKind::ReadByte { address, .. } if address.get() == device.registers().block_count
        )
    }

    /// The transaction one action submits.
    fn submit(action: &AicAction) -> SdioRequest {
        match action {
            AicAction::SubmitSdio(request) => request.clone(),
            other => panic!("expected a submitted transaction, got {other:?}"),
        }
    }

    #[test]
    fn a_write_completion_continues_the_pipeline_once_then_yields_to_the_scan() {
        // Three frames are queued, so the transmit side never runs dry: the
        // first completion continues the pipeline itself, and the bound then
        // hands the next advance back so the receive scan drains the firmware.
        let now = MonotonicTime::default();
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 500);
        device
            .data
            .tx
            .enqueue(TxToken::new(2), vec![0; 500])
            .unwrap();
        device
            .data
            .tx
            .enqueue(TxToken::new(3), vec![0; 500])
            .unwrap();

        let flow = submit(&device.advance(AicInput::tick(now)));
        let write = submit(&device.advance(complete(&flow, SdioResponse::Byte(16), now)));
        assert!(matches!(write.kind, SdioRequestKind::Write { .. }));

        // The completion is published, and the queued frame is written without
        // waiting for the runtime to come back for it.
        let completed = device.advance(complete(&write, SdioResponse::Unit, now));
        assert!(matches!(completed, AicAction::Event(_)));
        // The continuation is the only path that arms the next write here; the
        // drive path would have to wait for another advance to form it.
        assert!(
            matches!(
                device.io.next,
                Some((IoPurpose::TransmitData, SdioRequestKind::Write { .. }))
            ),
            "the completion armed the next write: {:?}",
            device.io.next
        );
        let chained = submit(&device.advance(AicInput::tick(now)));
        assert!(
            matches!(chained.kind, SdioRequestKind::Write { .. }),
            "the completion continues the pipeline with the queued frame"
        );
        assert_eq!(device.data.tx.len(), 1, "the continuation took one frame");

        // A card interrupt arms the scan while that write is out.  The next
        // completion does not continue again: the drive path runs, which is
        // what lets the receive side have its turn.
        let completed = device.advance(complete(&chained, SdioResponse::Unit, now));
        assert!(matches!(completed, AicAction::Event(_)));
        device.request_receive_scan();
        let scan = device.advance(AicInput::tick(now));
        assert!(
            starts_receive_scan(&scan, &device),
            "the bound yields the advance to the receive scan: {scan:?}"
        );
    }

    #[test]
    fn a_write_completion_without_a_cached_credit_reads_the_flow_register_first() {
        // The completion continues the pipeline even when the cached reading
        // was spent: the flow-control read goes out first and its own
        // completion continues into the write.
        let now = MonotonicTime::default();
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 500);
        device.data.tx_credits = Some(DATA_TX_RESERVED_CREDITS + 1);
        device
            .data
            .tx
            .enqueue(TxToken::new(2), vec![0; 500])
            .unwrap();

        let write = submit(&device.advance(AicInput::tick(now)));
        assert!(matches!(write.kind, SdioRequestKind::Write { .. }));
        let completed = device.advance(complete(&write, SdioResponse::Unit, now));
        assert!(matches!(completed, AicAction::Event(_)));
        assert!(
            matches!(device.io.next, Some((IoPurpose::TransmitFlow, _))),
            "a spent cache arms the credit read: {:?}",
            device.io.next
        );

        let next = submit(&device.advance(AicInput::tick(now)));
        assert!(
            matches!(
                next.kind,
                SdioRequestKind::ReadByte { address, .. }
                    if address.get() == device.registers().flow_control
            ),
            "a spent cache re-reads the credit register before writing: {:?}",
            next.kind
        );
        let write = submit(&device.advance(complete(&next, SdioResponse::Byte(16), now)));
        assert!(matches!(write.kind, SdioRequestKind::Write { .. }));
    }

    #[test]
    fn cancellation_drops_the_armed_continuation_instead_of_writing_it() {
        // A cancel landing between a write completion and the next advance used
        // to leave the armed write queued: it went out after the cancel, and
        // its own completion found no write to match, which failed the device.
        let now = MonotonicTime::default();
        let mut device = ready_transmitter(ChipVariant::Aic8800D80, 500);
        device
            .data
            .tx
            .enqueue(TxToken::new(2), vec![0; 500])
            .unwrap();
        // Cancelling is a control request, so the control session it belongs to
        // has to be open.
        let mut control = super::super::control::build(
            ControlRequest::Connect {
                ssid: b"network".to_vec(),
                pmk: None,
                entropy: None,
            },
            [2, 0, 0, 0, 0, 1],
            Some(0),
        )
        .unwrap();
        control.commands.clear();
        device.lifecycle.control = Some(control);

        let flow = submit(&device.advance(AicInput::tick(now)));
        let write = submit(&device.advance(complete(&flow, SdioResponse::Byte(16), now)));
        let completed = device.advance(complete(&write, SdioResponse::Unit, now));
        assert!(matches!(completed, AicAction::Event(_)));
        assert!(device.io.next.is_some(), "the completion armed a write");

        // The cancel arrives before the next advance emits anything.
        let cancelled = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::Control(ControlRequest::Cancel)),
        });
        assert!(matches!(cancelled, AicAction::Event(_)));
        assert!(device.io.next.is_none(), "the armed write was dropped");

        // The next advance must not transmit the dropped write: a stale one
        // would complete with no active write and fail the device.
        let next = device.advance(AicInput::tick(now));
        assert!(
            !matches!(next, AicAction::SubmitSdio(_)),
            "nothing is transmitted after the cancel: {next:?}"
        );
        assert_eq!(device.lifecycle.state, AicState::Ready);
    }

    #[test]
    fn v3_other_interrupt_acknowledges_the_dev_to_host_soft_irq() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Ready;
        device.request_receive_scan();

        let AicAction::SubmitSdio(count) =
            device.advance(AicInput::tick(MonotonicTime::from_nanos(0)))
        else {
            panic!("expected the receive count read")
        };
        assert!(matches!(
            count.kind,
            SdioRequestKind::ReadByte { function, address } if function.get() == 1
                && address.get() == device.registers().block_count
        ));

        let AicAction::SubmitSdio(ack) = device.advance(complete(
            &count,
            SdioResponse::Byte(0x83),
            MonotonicTime::from_nanos(1),
        )) else {
            panic!("expected the interrupt-pending ack read after an OTHER interrupt")
        };
        assert!(matches!(
            ack.kind,
            SdioRequestKind::ReadByte { function, address } if function.get() == 1
                && address.get() == device.registers().sleep_status.expect("v3 sleep status")
        ));

        let AicAction::SubmitSdio(clear) = device.advance(complete(
            &ack,
            SdioResponse::Byte(0x11),
            MonotonicTime::from_nanos(2),
        )) else {
            panic!("expected the soft-irq clear write after the pending read")
        };
        assert!(matches!(
            clear.kind,
            SdioRequestKind::WriteByte {
                function,
                address,
                value: 0x10,
                ..
            } if function.get() == 1
                && address.get() == device.registers().sleep_status.expect("v3 sleep status")
        ));

        let _ = device.advance(complete(
            &clear,
            SdioResponse::Byte(0x00),
            MonotonicTime::from_nanos(3),
        ));
    }

    #[test]
    fn v1_receive_counts_never_trigger_the_v3_other_interrupt_ack() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Ready;
        device.request_receive_scan();

        let AicAction::SubmitSdio(count) =
            device.advance(AicInput::tick(MonotonicTime::from_nanos(0)))
        else {
            panic!("expected the receive count read")
        };
        let AicAction::SubmitSdio(next) = device.advance(complete(
            &count,
            SdioResponse::Byte(0x83),
            MonotonicTime::from_nanos(1),
        )) else {
            panic!("expected the byte-mode length read")
        };
        assert!(matches!(
            next.kind,
            SdioRequestKind::ReadByte { address, .. } if address.get() == device.registers().byte_mode_length
        ));
    }

    #[test]
    fn v3_other_ack_re_reads_the_same_path_count_until_empty() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Ready;
        device.request_receive_scan();

        let AicAction::SubmitSdio(count) =
            device.advance(AicInput::tick(MonotonicTime::from_nanos(0)))
        else {
            panic!("expected the receive count read")
        };
        let AicAction::SubmitSdio(ack) = device.advance(complete(
            &count,
            SdioResponse::Byte(0x83),
            MonotonicTime::from_nanos(1),
        )) else {
            panic!("expected the interrupt-pending ack read")
        };
        let AicAction::SubmitSdio(clear) = device.advance(complete(
            &ack,
            SdioResponse::Byte(0x11),
            MonotonicTime::from_nanos(2),
        )) else {
            panic!("expected the soft-irq clear write")
        };
        // The vendor D80 handler re-reads the interrupt status after the soft
        // IRQ acknowledgement; the scan must stay on the same path instead of
        // advancing so queued data is drained before the scan ends.
        let AicAction::SubmitSdio(recount) = device.advance(complete(
            &clear,
            SdioResponse::Byte(0x00),
            MonotonicTime::from_nanos(3),
        )) else {
            panic!("expected the same-path count re-read after the soft IRQ acknowledgement")
        };
        assert!(matches!(
            recount.kind,
            SdioRequestKind::ReadByte { address, .. } if address.get()
                == device.registers().block_count
        ));
    }
}
