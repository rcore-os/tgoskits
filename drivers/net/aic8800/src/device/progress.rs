use alloc::{vec, vec::Vec};

use super::*;

#[cfg(test)]
const NANOS_PER_MILLISECOND: u64 = 1_000_000;

impl AicDevice {
    /// Begins initialization. No SDIO work is executed by this method.
    pub fn start(&mut self, now: MonotonicTime) -> Result<(), AicError> {
        if self.lifecycle.state != AicState::Stopped {
            return Err(AicError::Busy);
        }
        self.observe_time(now)?;
        self.lifecycle.state = AicState::Starting;
        self.lifecycle.startup = Some(StartupState::new());
        Ok(())
    }

    /// Advances at most one externally visible transition.
    pub fn advance(&mut self, input: AicInput) -> AicAction {
        if let Err(error) = self.observe_time(input.now) {
            return self.fail(error);
        }
        if let Some(event) = input.event
            && let Err(error) = self.consume_input(event, input.now)
        {
            return self.fail(error);
        }
        self.data.promote_pending_completions();
        if let Some(event) = self.data.pop_event() {
            return AicAction::Event(event);
        }
        if self.lifecycle.cancel_pending
            && let Some(pending) = &self.io.pending
        {
            return AicAction::AbortSdio {
                request_id: pending.id,
            };
        }
        if self.io.pending.is_some() {
            return AicAction::WaitForInterrupt;
        }
        if let Some((purpose, kind)) = self.io.next.take() {
            return self.emit(purpose, kind);
        }
        if let Some(deadline) = self.lifecycle.retry_at {
            if input.now < deadline {
                return AicAction::RetryAt(deadline);
            }
            self.lifecycle.retry_at = None;
        }
        match self.lifecycle.state {
            AicState::Starting => self.drive_startup(input.now),
            AicState::Ready => self.drive_ready(input.now),
            AicState::Stopping => self.drive_shutdown(),
            AicState::Stopped | AicState::Failed => AicAction::Idle,
        }
    }

    fn observe_time(&mut self, now: MonotonicTime) -> Result<(), AicError> {
        if now < self.lifecycle.last_time {
            return Err(AicError::NonMonotonicTime);
        }
        self.lifecycle.last_time = now;
        Ok(())
    }

    fn consume_input(&mut self, event: AicInputEvent, now: MonotonicTime) -> Result<(), AicError> {
        match event {
            AicInputEvent::Sdio(completion) => self.consume_completion(completion, now),
            AicInputEvent::Irq(snapshot) => {
                if let Some(error) = snapshot.error {
                    return Err(AicError::Sdio(error));
                }
                if snapshot.sequence > self.io.last_irq_sequence {
                    self.io.last_irq_sequence = snapshot.sequence;
                    if snapshot.card_interrupt {
                        self.request_receive_scan();
                    }
                }
                Ok(())
            }
            AicInputEvent::Control(request) => self.consume_control(request, now),
            AicInputEvent::Tx { token, frame } => {
                if self.lifecycle.state != AicState::Ready {
                    return Err(AicError::Busy);
                }
                if self.data.tx.enqueue(token, frame).is_err() {
                    // A full transmit queue is the same over-capacity case the
                    // batch form reports complete, not a device failure.
                    self.complete_write_tokens(vec![token]);
                }
                Ok(())
            }
            AicInputEvent::TxBatch(frames) => {
                if self.lifecycle.state != AicState::Ready {
                    return Err(AicError::Busy);
                }
                let mut frames = frames.into_iter();
                while let Some((token, frame)) = frames.next() {
                    if self.data.tx.enqueue(token, frame).is_err() {
                        // The queue is full: drop this frame and the rest of the
                        // batch, releasing their tokens so their DMA buffers
                        // return to the runtime instead of failing the device.
                        self.complete_write_tokens(
                            core::iter::once(token)
                                .chain(frames.map(|(token, _)| token))
                                .collect(),
                        );
                        return Ok(());
                    }
                }
                Ok(())
            }
        }
    }

    fn consume_control(
        &mut self,
        request: ControlRequest,
        _now: MonotonicTime,
    ) -> Result<(), AicError> {
        match request {
            ControlRequest::Cancel => {
                if self.lifecycle.control.is_none() && self.lifecycle.state != AicState::Starting {
                    return Err(AicError::InvalidControlRequest);
                }
                self.lifecycle.cancel_pending = true;
                if self.io.pending.is_none() {
                    self.finish_cancel();
                }
                Ok(())
            }
            ControlRequest::Shutdown => {
                // Nothing queued is transmitted after this point, including the
                // write a continuation queued.
                self.discard_pending_transmit();
                self.lifecycle.state = AicState::Stopping;
                self.lifecycle.control = None;
                self.lifecycle.mailbox = None;
                self.lifecycle.cancel_pending = false;
                self.data.link.clear_peer();
                self.data.clear_internal_tx();
                Ok(())
            }
            operation => {
                if self.lifecycle.state != AicState::Ready || self.lifecycle.control.is_some() {
                    return Err(AicError::Busy);
                }
                let mac = self
                    .data
                    .link
                    .mac_address()
                    .ok_or(AicError::InvalidMacAddress)?;
                self.lifecycle.control = Some(super::control::build(
                    operation,
                    mac,
                    self.data.link.interface_index(),
                )?);
                Ok(())
            }
        }
    }

    fn consume_completion(
        &mut self,
        completion: SdioCompletion,
        now: MonotonicTime,
    ) -> Result<(), AicError> {
        let pending = self.io.pending.take().ok_or(AicError::CompletionMismatch)?;
        if pending.id != completion.request_id {
            self.io.pending = Some(pending);
            return Err(AicError::CompletionMismatch);
        }
        if self.lifecycle.cancel_pending {
            if let Err(error) = completion.result
                && error != SdioFailure::Aborted
            {
                return Err(AicError::Sdio(error));
            }
            self.finish_cancel();
            return Ok(());
        }
        let response = completion.result.map_err(AicError::Sdio)?;
        // The command mailbox and lifecycle commands can take buffers from the
        // firmware packet pool that a cached credit counted on the profiles
        // where they travel over the data FIFO, so the reading is dropped
        // whenever command work completes.  Receive work never takes pool
        // buffers and keeps the cached credit valid.
        if matches!(
            pending.purpose,
            IoPurpose::Startup
                | IoPurpose::Shutdown
                | IoPurpose::MailboxFlow
                | IoPurpose::MailboxWrite
        ) {
            self.clear_tx_credits();
        }
        match pending.purpose {
            IoPurpose::Startup => self.consume_startup_response(response, now),
            IoPurpose::MailboxFlow | IoPurpose::MailboxWrite => {
                self.consume_mailbox_response(pending.purpose, response, now)
            }
            IoPurpose::ReceiveCount(path) => self.consume_receive_count(path, response),
            IoPurpose::ReceiveByteLength(path) => self.consume_receive_byte_length(path, response),
            IoPurpose::ReceiveData(path) => self.consume_receive_data(path, response),
            IoPurpose::ReceiveOtherAck(path) => self.consume_receive_other_ack(path, response),
            IoPurpose::ReceiveOtherClear(path) => self.consume_receive_other_clear(path, response),
            IoPurpose::TransmitFlow => self.consume_transmit_flow(response, now),
            IoPurpose::TransmitData => {
                self.consume_transmit_data(response)?;
                self.continue_transmit_pipeline(now);
                Ok(())
            }
            IoPurpose::Shutdown => {
                expect_write_readback(response, 0)?;
                self.lifecycle.state = AicState::Stopped;
                let _ = self.data.push_event(AicEvent::Stopped);
                Ok(())
            }
        }
    }

    pub(super) fn emit(&mut self, purpose: IoPurpose, kind: SdioRequestKind) -> AicAction {
        let id = self.io.next_request_id;
        self.io.next_request_id = self.io.next_request_id.wrapping_add(1).max(1);
        self.io.pending = Some(PendingIo { id, purpose });
        AicAction::SubmitSdio(SdioRequest { id, kind })
    }

    fn drive_shutdown(&mut self) -> AicAction {
        if let Some(pending) = &self.io.pending {
            return AicAction::AbortSdio {
                request_id: pending.id,
            };
        }
        // Nothing queued will be transmitted any more, so the packets the
        // device still holds are reported complete and their buffers return to
        // the runtime before the shutdown write goes out.
        let tokens = self.take_active_write_tokens();
        self.complete_write_tokens(tokens);
        let queued: Vec<_> = self.data.tx.drain_tokens().collect();
        self.complete_write_tokens(queued);
        if let Some(event) = self.data.pop_event() {
            return AicAction::Event(event);
        }
        self.emit(
            IoPurpose::Shutdown,
            write_byte(self.data_function(), self.registers().interrupt_enable, 0),
        )
    }

    fn finish_cancel(&mut self) {
        // The aborted write is dropped rather than retried, so its packets are
        // reported complete and their buffers return to the runtime.
        let tokens = self.take_active_write_tokens();
        self.complete_write_tokens(tokens);
        // The write a continuation queued at the previous completion would
        // otherwise be the next thing emitted, and its own completion would
        // find no write to match.
        self.discard_pending_transmit();
        self.lifecycle.cancel_pending = false;
        self.lifecycle.mailbox = None;
        self.lifecycle.control = None;
        self.data.link.clear_peer();
        self.data.clear_internal_tx();
        self.clear_tx_credits();
        if self.lifecycle.state == AicState::Starting {
            self.lifecycle.startup = None;
            self.lifecycle.state = AicState::Stopped;
        }
        let _ = self.data.push_event(AicEvent::ControlCancelled);
    }

    pub(super) fn fail(&mut self, error: AicError) -> AicAction {
        self.lifecycle.state = AicState::Failed;
        self.io.pending = None;
        self.io.next = None;
        self.lifecycle.mailbox = None;
        self.lifecycle.control = None;
        self.data.link.clear_peer();
        for token in self.take_active_write_tokens() {
            let _ = self.data.push_event(AicEvent::TransmitComplete(token));
        }
        self.data.clear_internal_tx();
        let tokens: Vec<_> = self.data.tx.drain_tokens().collect();
        for token in tokens {
            let _ = self.data.push_event(AicEvent::TransmitComplete(token));
        }
        let stalled: Vec<_> = self.data.pending_completions.drain(..).collect();
        for token in stalled {
            let _ = self.data.push_event(AicEvent::TransmitComplete(token));
        }
        // The pop below always finds an event: a publication is refused only
        // when the queue already holds `RX_CAPACITY` events and none of them is
        // a receive frame to evict, and then the terminal event waits its turn
        // while an earlier event is returned.
        let _ = self.data.push_event(AicEvent::Failed(error));
        AicAction::Event(
            self.data
                .pop_event()
                .expect("failure always publishes at least one terminal event"),
        )
    }

    pub(super) const fn command_function(&self) -> u8 {
        self.profile.command_function()
    }

    pub(super) const fn data_function(&self) -> u8 {
        self.profile.data_function()
    }

    pub(super) const fn transport_uses_header_crc(&self) -> bool {
        self.profile.transport_header().uses_crc()
    }

    pub(super) const fn registers(&self) -> crate::registers::RegisterMap {
        self.profile.registers()
    }

    pub(super) const fn transport_generation(&self) -> crate::profile::TransportGeneration {
        self.profile.transport()
    }

    pub(super) const fn firmware_profile(&self) -> crate::profile::FirmwareProfile {
        self.profile.firmware()
    }

    pub(super) const fn mailbox_flow_policy(&self) -> crate::profile::MailboxFlowPolicy {
        self.profile.mailbox_flow()
    }

    pub(super) const fn startup_function(&self, index: usize) -> Option<u8> {
        self.profile.function(index)
    }

    pub(super) const fn receive_path(&self, index: usize) -> Option<RxPath> {
        match index {
            0 => Some(RxPath::Command),
            1 if self.command_function() != self.data_function() => Some(RxPath::Data),
            _ => None,
        }
    }

    pub(super) const fn receive_function(&self, path: RxPath) -> u8 {
        match path {
            RxPath::Command => self.command_function(),
            RxPath::Data => self.data_function(),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::common::{ChipVariant, SDIOWIFI_FUNC_BLOCKSIZE};

    fn time(ms: u64) -> MonotonicTime {
        MonotonicTime::from_nanos(ms * NANOS_PER_MILLISECOND)
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

    #[test]
    fn startup_begins_with_protocol_owned_function_lifecycle() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.start(time(0)).unwrap();
        let AicAction::SubmitSdio(block_size) = device.advance(AicInput::tick(time(0))) else {
            panic!("expected block-size configuration")
        };
        assert!(matches!(
            block_size.kind,
            SdioRequestKind::SetBlockSize { function, block_size }
                if function.get() == 1 && block_size.get() == SDIOWIFI_FUNC_BLOCKSIZE
        ));
        let AicAction::SubmitSdio(enable) =
            device.advance(complete(&block_size, SdioResponse::Unit, time(0)))
        else {
            panic!("expected function enable")
        };
        assert!(matches!(
            enable.kind,
            SdioRequestKind::EnableFunction(number) if number.get() == 1
        ));
    }

    #[test]
    fn dc_configures_both_sdio_functions_before_vendor_setup() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.start(time(0)).unwrap();

        let AicAction::SubmitSdio(function_one_block) = device.advance(AicInput::tick(time(0)))
        else {
            panic!("expected function-one block-size configuration")
        };
        assert!(matches!(
            function_one_block.kind,
            SdioRequestKind::SetBlockSize { function, .. } if function.get() == 1
        ));

        let AicAction::SubmitSdio(function_one_enable) =
            device.advance(complete(&function_one_block, SdioResponse::Unit, time(0)))
        else {
            panic!("expected function-one enable")
        };
        assert!(matches!(
            function_one_enable.kind,
            SdioRequestKind::EnableFunction(function) if function.get() == 1
        ));

        let AicAction::SubmitSdio(function_two_block) =
            device.advance(complete(&function_one_enable, SdioResponse::Unit, time(0)))
        else {
            panic!("expected function-two block-size configuration")
        };
        assert!(matches!(
            function_two_block.kind,
            SdioRequestKind::SetBlockSize { function, .. } if function.get() == 2
        ));
    }

    #[test]
    fn dc_enables_cccr_interrupts_for_both_functions() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.start(time(0)).unwrap();

        let AicAction::SubmitSdio(function_one_block) = device.advance(AicInput::tick(time(0)))
        else {
            panic!("expected function-one block-size configuration")
        };
        let AicAction::SubmitSdio(function_one_enable) =
            device.advance(complete(&function_one_block, SdioResponse::Unit, time(0)))
        else {
            panic!("expected function-one enable")
        };
        let AicAction::SubmitSdio(function_two_block) =
            device.advance(complete(&function_one_enable, SdioResponse::Unit, time(0)))
        else {
            panic!("expected function-two block-size configuration")
        };
        let AicAction::SubmitSdio(function_two_enable) =
            device.advance(complete(&function_two_block, SdioResponse::Unit, time(0)))
        else {
            panic!("expected function-two enable")
        };
        let AicAction::SubmitSdio(function_one_interrupt) =
            device.advance(complete(&function_two_enable, SdioResponse::Unit, time(0)))
        else {
            panic!("expected function-one CCCR interrupt enable")
        };
        assert!(matches!(
            function_one_interrupt.kind,
            SdioRequestKind::EnableFunctionInterrupt(function) if function.get() == 1
        ));

        let AicAction::SubmitSdio(function_two_interrupt) = device.advance(complete(
            &function_one_interrupt,
            SdioResponse::Unit,
            time(0),
        )) else {
            panic!("expected function-two CCCR interrupt enable")
        };
        assert!(matches!(
            function_two_interrupt.kind,
            SdioRequestKind::EnableFunctionInterrupt(function) if function.get() == 2
        ));
    }

    #[test]
    fn retry_deadline_does_not_advance_early() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.start(time(0)).unwrap();
        device.lifecycle.retry_at = Some(time(10));
        assert_eq!(
            device.advance(AicInput::tick(time(9))),
            AicAction::RetryAt(time(10))
        );
    }

    #[test]
    fn cancellation_requests_abort_for_the_exact_active_transaction() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.start(time(0)).unwrap();
        let AicAction::SubmitSdio(request) = device.advance(AicInput::tick(time(0))) else {
            panic!("expected request")
        };
        let action = device.advance(AicInput {
            now: time(1),
            event: Some(AicInputEvent::Control(ControlRequest::Cancel)),
        });
        assert_eq!(
            action,
            AicAction::AbortSdio {
                request_id: request.id
            }
        );
    }

    #[test]
    fn aborted_completion_finishes_cancellation_without_failing_device() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.start(time(0)).unwrap();
        let AicAction::SubmitSdio(request) = device.advance(AicInput::tick(time(0))) else {
            panic!("expected request")
        };
        assert!(matches!(
            device.advance(AicInput {
                now: time(1),
                event: Some(AicInputEvent::Control(ControlRequest::Cancel)),
            }),
            AicAction::AbortSdio { .. }
        ));

        assert_eq!(
            device.advance(AicInput {
                now: time(1),
                event: Some(AicInputEvent::Sdio(SdioCompletion {
                    request_id: request.id,
                    result: Err(SdioFailure::Aborted),
                })),
            }),
            AicAction::Event(AicEvent::ControlCancelled)
        );
        assert_eq!(device.state(), AicState::Stopped);
    }

    #[test]
    fn non_monotonic_input_fails_closed() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.start(time(2)).unwrap();
        assert_eq!(
            device.advance(AicInput::tick(time(1))),
            AicAction::Event(AicEvent::Failed(AicError::NonMonotonicTime))
        );
    }

    #[test]
    fn shutdown_reclaims_a_queued_aggregate_continuation_before_sdio_shutdown() {
        let now = time(0);
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Ready;
        device
            .set_tx_aggregation(super::owner::TxAggregation::new(2, 4096))
            .unwrap();
        device.data.tx_credits = Some(16);
        device.data.link.install_interface(0).unwrap();
        device
            .data
            .link
            .install_peer(0, 1, [2, 0, 0, 0, 0, 1])
            .unwrap();
        for token in 1..=4 {
            device
                .data
                .tx
                .enqueue(TxToken::new(token), vec![0; 60])
                .unwrap();
        }

        let AicAction::SubmitSdio(first_write) = device.advance(AicInput::tick(now)) else {
            panic!("expected the first aggregate write")
        };
        assert_eq!(device.data.active_tx.as_ref().unwrap().packets(), 2);
        assert!(matches!(
            device.advance(complete(&first_write, SdioResponse::Unit, now)),
            AicAction::Event(AicEvent::TransmitAggregateComplete(tokens))
                if tokens == vec![TxToken::new(1), TxToken::new(2)]
        ));
        assert_eq!(device.data.active_tx.as_ref().unwrap().packets(), 2);
        assert!(
            device.io.next.is_some(),
            "continuation is armed but not submitted"
        );

        let shutdown = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::Control(ControlRequest::Shutdown)),
        });
        assert!(
            matches!(shutdown, AicAction::Event(AicEvent::TransmitAggregateComplete(tokens))
            if tokens == vec![TxToken::new(3), TxToken::new(4)])
        );
        assert!(
            device.io.next.is_none(),
            "shutdown discards the queued continuation"
        );
        assert!(matches!(
            device.advance(AicInput::tick(now)),
            AicAction::SubmitSdio(SdioRequest {
                kind: SdioRequestKind::WriteByte { .. },
                ..
            })
        ));
    }

    #[test]
    fn failure_reclaims_active_and_queued_transmit_tokens_before_terminal_error() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Ready;
        let active = TxToken::new(7);
        let queued = TxToken::new(8);
        device.data.active_tx = Some(ActiveTx::new(
            super::owner::TxCompletion::User(active),
            vec![1],
            1,
        ));
        device.data.tx.enqueue(queued, vec![2]).unwrap();

        assert_eq!(
            device.fail(AicError::Sdio(SdioFailure::Bus)),
            AicAction::Event(AicEvent::TransmitComplete(active))
        );
        assert_eq!(
            device.advance(AicInput::tick(time(0))),
            AicAction::Event(AicEvent::TransmitComplete(queued))
        );
        assert_eq!(
            device.advance(AicInput::tick(time(0))),
            AicAction::Event(AicEvent::Failed(AicError::Sdio(SdioFailure::Bus)))
        );
    }
}
