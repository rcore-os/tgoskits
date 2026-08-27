use alloc::vec::Vec;
use core::time::Duration;

use super::*;
use crate::{
    protocol::{BLOCK_SIZE, command_frame, confirmation_payload, debug_command_frame},
    registers::{flow_credits, interrupt_block_count},
};

const MAILBOX_TIMEOUT: Duration = Duration::from_secs(5);
const MAILBOX_FLOW_RETRY: Duration = Duration::from_millis(1);
const MAILBOX_SETTLE: Duration = Duration::from_millis(2);
const MAX_MAILBOX_FLOW_RETRIES: u16 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MailboxPhase {
    Flow,
    Write,
    Settle,
    Count,
    Read { length: usize },
    Complete,
}

pub(super) struct MailboxState {
    frame: Vec<u8>,
    expected_message_id: u16,
    phase: MailboxPhase,
    deadline: MonotonicTime,
    retry_at: Option<MonotonicTime>,
    flow_retries: u16,
    result: Option<Vec<u8>>,
}

impl AicDevice {
    pub(super) fn drive_mailbox(&mut self, now: MonotonicTime) -> AicAction {
        let Some(mailbox) = self.lifecycle.mailbox.as_mut() else {
            return AicAction::Idle;
        };
        if now >= mailbox.deadline {
            return self.fail(AicError::MailboxTimeout);
        }
        if let Some(retry_at) = mailbox.retry_at {
            if now < retry_at {
                return AicAction::RetryAt(retry_at);
            }
            mailbox.retry_at = None;
        }
        match mailbox.phase {
            MailboxPhase::Flow => self.emit(
                IoPurpose::MailboxFlow,
                read_byte(1, self.registers.flow_control),
            ),
            MailboxPhase::Write => {
                let frame = mailbox.frame.clone();
                self.emit(
                    IoPurpose::MailboxWrite,
                    write_fifo(self.command_function(), self.registers.write_fifo, frame),
                )
            }
            MailboxPhase::Settle => {
                mailbox.phase = MailboxPhase::Count;
                self.drive_mailbox(now)
            }
            MailboxPhase::Count => self.emit(
                IoPurpose::MailboxCount,
                read_byte(self.command_function(), self.registers.block_count),
            ),
            MailboxPhase::Read { length } => self.emit(
                IoPurpose::MailboxRead,
                read_fifo(self.command_function(), self.registers.read_fifo, length),
            ),
            MailboxPhase::Complete => match self.complete_mailbox() {
                Ok(()) => self.drive_startup_or_ready(now),
                Err(error) => self.fail(error),
            },
        }
    }

    pub(super) fn consume_mailbox_response(
        &mut self,
        purpose: IoPurpose,
        response: SdioResponse,
        now: MonotonicTime,
    ) -> Result<(), AicError> {
        let mailbox = self
            .lifecycle
            .mailbox
            .as_mut()
            .ok_or(AicError::CompletionMismatch)?;
        match purpose {
            IoPurpose::MailboxFlow => {
                let flow = flow_credits(expect_byte(response)?);
                if flow == 0 {
                    mailbox.flow_retries = mailbox.flow_retries.saturating_add(1);
                    if mailbox.flow_retries >= MAX_MAILBOX_FLOW_RETRIES {
                        return Err(AicError::MailboxTimeout);
                    }
                    mailbox.retry_at = Some(now.after(MAILBOX_FLOW_RETRY));
                } else {
                    mailbox.phase = MailboxPhase::Write;
                }
            }
            IoPurpose::MailboxWrite => {
                expect_unit(response)?;
                mailbox.phase = MailboxPhase::Settle;
                mailbox.retry_at = Some(now.after(MAILBOX_SETTLE));
            }
            IoPurpose::MailboxCount => match interrupt_block_count(expect_byte(response)?) {
                None | Some(0) => {
                    mailbox.retry_at = Some(now.after(MAILBOX_FLOW_RETRY));
                }
                Some(count) => {
                    mailbox.phase = MailboxPhase::Read {
                        length: usize::from(count) * BLOCK_SIZE,
                    };
                }
            },
            IoPurpose::MailboxRead => {
                let data = expect_data(response)?;
                mailbox.result = Some(
                    confirmation_payload(&data, mailbox.expected_message_id)
                        .map_err(|_| AicError::MalformedResponse)?,
                );
                mailbox.phase = MailboxPhase::Complete;
            }
            _ => return Err(AicError::CompletionMismatch),
        }
        Ok(())
    }

    fn complete_mailbox(&mut self) -> Result<(), AicError> {
        let result = self
            .lifecycle
            .mailbox
            .take()
            .and_then(|mut mailbox| mailbox.result.take())
            .ok_or(AicError::MalformedResponse)?;
        if self.lifecycle.state == AicState::Starting {
            self.complete_startup_mailbox(result)
        } else if let Some(control) = self.lifecycle.control.as_mut() {
            control.commands.pop_front();
            if control.commands.is_empty() {
                self.lifecycle.control = None;
                self.data.events.push_back(AicEvent::ControlComplete);
            }
            Ok(())
        } else {
            Err(AicError::CompletionMismatch)
        }
    }

    fn drive_startup_or_ready(&mut self, now: MonotonicTime) -> AicAction {
        if self.lifecycle.state == AicState::Starting {
            self.drive_startup(now)
        } else {
            self.drive_ready(now)
        }
    }

    pub(super) fn begin_debug_mailbox(
        &mut self,
        message_id: u16,
        payload: &[u8],
        now: MonotonicTime,
    ) {
        self.lifecycle.mailbox = Some(MailboxState {
            frame: debug_command_frame(message_id, payload, self.chip.is_v3()),
            expected_message_id: message_id + 1,
            phase: MailboxPhase::Flow,
            deadline: now.after(MAILBOX_TIMEOUT),
            retry_at: None,
            flow_retries: 0,
            result: None,
        });
    }

    pub(super) fn begin_lmac_mailbox(
        &mut self,
        message_id: u16,
        destination: u16,
        payload: &[u8],
        expected_message_id: u16,
        now: MonotonicTime,
    ) {
        self.lifecycle.mailbox = Some(MailboxState {
            frame: command_frame(message_id, destination, payload, self.chip.is_v3()),
            expected_message_id,
            phase: MailboxPhase::Flow,
            deadline: now.after(MAILBOX_TIMEOUT),
            retry_at: None,
            flow_retries: 0,
            result: None,
        });
    }
}
