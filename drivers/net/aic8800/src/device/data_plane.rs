use alloc::vec::Vec;
use core::time::Duration;

use super::*;
use crate::{
    protocol::BLOCK_SIZE,
    registers::{flow_credits, interrupt_block_count},
    rx::{ParsedFrame, parse_fifo},
};

const IO_RETRY: Duration = Duration::from_millis(1);

impl AicDevice {
    pub(super) fn drive_ready(&mut self, now: MonotonicTime) -> AicAction {
        if self.lifecycle.mailbox.is_some() {
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
        if self.io.irq_pending {
            self.io.irq_pending = false;
            return self.emit(
                IoPurpose::ReceiveCount,
                read_byte(1, self.registers.block_count),
            );
        }
        self.prepare_next_transmit();
        if self.data.active_tx.is_some() {
            return self.emit(
                IoPurpose::TransmitFlow,
                read_byte(1, self.registers.flow_control),
            );
        }
        self.data
            .events
            .pop_front()
            .map_or(AicAction::WaitForInterrupt, AicAction::Event)
    }

    pub(super) fn consume_receive_count(&mut self, response: SdioResponse) -> Result<(), AicError> {
        let Some(blocks) = interrupt_block_count(expect_byte(response)?) else {
            self.io.irq_pending = true;
            return Ok(());
        };
        if blocks != 0 {
            self.io.next = Some((
                IoPurpose::ReceiveData,
                read_fifo(
                    1,
                    self.registers.read_fifo,
                    usize::from(blocks) * BLOCK_SIZE,
                ),
            ));
        }
        Ok(())
    }

    pub(super) fn consume_receive_data(&mut self, response: SdioResponse) -> Result<(), AicError> {
        for frame in parse_fifo(&expect_data(response)?) {
            match frame {
                ParsedFrame::Data(frame) => {
                    if self.data.rx.push(frame.clone()) {
                        self.data.events.push_back(AicEvent::Receive(frame));
                    }
                }
                ParsedFrame::Confirmation {
                    message_id,
                    payload,
                }
                | ParsedFrame::Indication {
                    message_id,
                    payload,
                } => {
                    log::trace!(
                        "AIC message id={message_id:#06x}, payload={} bytes",
                        payload.len()
                    );
                }
            }
        }
        Ok(())
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
            write_fifo(1, self.registers.write_fifo, active.wire_frame.clone()),
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
        self.data
            .events
            .push_back(AicEvent::TransmitComplete(active.token));
        Ok(())
    }

    fn prepare_next_transmit(&mut self) {
        if self.data.active_tx.is_some() {
            return;
        }
        let Some(frame) = self.data.tx.take_wire_frame(
            self.data.interface_index,
            self.data.station_index,
            self.chip.is_v3(),
        ) else {
            return;
        };
        match frame {
            Ok((token, wire_frame)) => {
                self.data.active_tx = Some(ActiveTx { token, wire_frame });
            }
            Err(token) => self
                .data
                .events
                .push_back(AicEvent::TransmitComplete(token)),
        }
    }

    /// Removes one received Ethernet frame from the bounded core queue.
    pub fn take_received(&mut self) -> Option<Vec<u8>> {
        self.data.rx.pop()
    }
}
