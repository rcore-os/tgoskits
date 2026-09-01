use alloc::vec::Vec;
use core::time::Duration;

use super::*;
use crate::{
    profile::MailboxFlowPolicy,
    protocol::{
        DBG_MEM_BLOCK_WRITE_REQ, DBG_MEM_MASK_WRITE_REQ, DBG_MEM_READ_REQ, DBG_MEM_WRITE_REQ,
        DBG_START_APP_REQ, command_frame, debug_command_frame,
    },
    registers::flow_credits,
};

const MAILBOX_TIMEOUT: Duration = Duration::from_secs(5);
const MAILBOX_FLOW_RETRY: Duration = Duration::from_millis(1);
const MAX_MAILBOX_FLOW_RETRIES: u16 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MailboxPhase {
    Flow,
    Write,
    Confirmation,
    Complete,
}

impl From<MailboxPhase> for MailboxWaitPhase {
    fn from(phase: MailboxPhase) -> Self {
        match phase {
            MailboxPhase::Flow => Self::Flow,
            MailboxPhase::Write => Self::Write,
            MailboxPhase::Confirmation => Self::Confirmation,
            MailboxPhase::Complete => Self::Complete,
        }
    }
}

pub(super) struct MailboxState {
    frame: Vec<u8>,
    request: MailboxRequest,
    expected_message_id: u16,
    phase: MailboxPhase,
    deadline: MonotonicTime,
    retry_at: Option<MonotonicTime>,
    flow_retries: u16,
    result: Option<Vec<u8>>,
}

#[cfg(test)]
impl MailboxState {
    pub(super) fn confirmation_for_test(deadline: MonotonicTime) -> Self {
        Self {
            frame: Vec::new(),
            request: MailboxRequest::Lmac { message_id: 1 },
            expected_message_id: 2,
            phase: MailboxPhase::Confirmation,
            deadline,
            retry_at: None,
            flow_retries: 0,
            result: None,
        }
    }
}

impl AicDevice {
    pub(super) fn startup_confirmation_waiting(&self) -> bool {
        matches!(
            self.lifecycle.mailbox.as_ref().map(|mailbox| mailbox.phase),
            Some(MailboxPhase::Confirmation)
        )
    }

    pub(super) fn mailbox_waiting_for_receive(&self) -> bool {
        self.lifecycle.mailbox.as_ref().is_some_and(|mailbox| {
            mailbox.phase == MailboxPhase::Confirmation && self.io.receive.active
        })
    }

    pub(super) fn mailbox_timed_out(&self, now: MonotonicTime) -> bool {
        self.lifecycle.mailbox.as_ref().is_some_and(|mailbox| {
            mailbox.phase != MailboxPhase::Complete && now >= mailbox.deadline
        })
    }

    pub(super) fn drive_mailbox(&mut self, now: MonotonicTime) -> AicAction {
        if self.mailbox_timed_out(now) {
            let startup_stage = self.startup_stage_diagnostic();
            let mailbox = self
                .lifecycle
                .mailbox
                .as_ref()
                .expect("mailbox timeout was checked above");
            log::error!(
                "[wifi] AIC mailbox timeout: expected={:#06x} phase={:?} flow_retries={} \
                 startup_stage={}",
                mailbox.expected_message_id,
                mailbox.phase,
                mailbox.flow_retries,
                startup_stage.as_deref().unwrap_or("none")
            );
            let error = mailbox_timeout(mailbox);
            return self.fail(error);
        }
        let Some(mailbox) = self.lifecycle.mailbox.as_mut() else {
            return AicAction::Idle;
        };
        if let Some(retry_at) = mailbox.retry_at {
            if now < retry_at {
                return AicAction::RetryAt(retry_at);
            }
            mailbox.retry_at = None;
        }
        match mailbox.phase {
            MailboxPhase::Flow => self.emit(
                IoPurpose::MailboxFlow,
                read_byte(self.data_function(), self.registers().flow_control),
            ),
            MailboxPhase::Write => {
                let frame = mailbox.frame.clone();
                self.emit(
                    IoPurpose::MailboxWrite,
                    write_fifo(self.command_function(), self.registers().write_fifo, frame),
                )
            }
            MailboxPhase::Confirmation => AicAction::WaitForInterruptUntil(mailbox.deadline),
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
                        return Err(mailbox_timeout(mailbox));
                    }
                    mailbox.retry_at = Some(now.after(MAILBOX_FLOW_RETRY));
                } else {
                    mailbox.phase = MailboxPhase::Write;
                }
            }
            IoPurpose::MailboxWrite => {
                expect_unit(response)?;
                mailbox.phase = MailboxPhase::Confirmation;
            }
            _ => return Err(AicError::CompletionMismatch),
        }
        Ok(())
    }

    pub(super) fn accept_mailbox_confirmation(
        &mut self,
        message_id: u16,
        payload: Vec<u8>,
    ) -> Result<(), AicError> {
        let mailbox = self
            .lifecycle
            .mailbox
            .as_mut()
            .ok_or(AicError::CompletionMismatch)?;
        if mailbox.phase != MailboxPhase::Confirmation {
            return Err(AicError::CompletionMismatch);
        }
        if message_id != mailbox.expected_message_id {
            return Err(AicError::UnexpectedConfirmation {
                expected_message_id: mailbox.expected_message_id,
                actual_message_id: message_id,
            });
        }
        mailbox.result = Some(payload);
        mailbox.phase = MailboxPhase::Complete;
        Ok(())
    }

    fn complete_mailbox(&mut self) -> Result<(), AicError> {
        let mut mailbox = self
            .lifecycle
            .mailbox
            .take()
            .ok_or(AicError::MalformedResponse)?;
        let result = mailbox
            .result
            .take()
            .ok_or(AicError::MalformedMailboxResponse {
                request: mailbox.request,
                expected_message_id: mailbox.expected_message_id,
                payload_length: 0,
            })?;
        let result_length = result.len();
        let completion = if self.lifecycle.state == AicState::Starting {
            let result_length = result.len();
            let result_header = result[..result.len().min(8)].to_vec();
            self.complete_startup_mailbox(result).inspect_err(|error| {
                self.log_startup_confirmation_error(result_length, &result_header, error);
            })
        } else {
            self.complete_control_mailbox(result)
        };
        completion.map_err(|error| {
            if error == AicError::MalformedResponse {
                AicError::MalformedMailboxResponse {
                    request: mailbox.request,
                    expected_message_id: mailbox.expected_message_id,
                    payload_length: result_length,
                }
            } else {
                error
            }
        })
    }

    fn complete_control_mailbox(&mut self, result: Vec<u8>) -> Result<(), AicError> {
        use super::control::{ConnectPhase, ControlOperation};
        use crate::lmac::{
            ME_SET_CONTROL_PORT_CFM, MM_KEY_ADD_CFM, SM_CONNECT_CFM, SM_DISCONNECT_CFM,
        };

        let control = self
            .lifecycle
            .control
            .as_mut()
            .ok_or(AicError::CompletionMismatch)?;
        let expected = control
            .commands
            .front()
            .map(|command| command.expected_message_id)
            .ok_or(AicError::CompletionMismatch)?;
        let mut finish = false;
        let mut open_control_port = false;
        let mut m4 = None;
        match expected {
            SM_DISCONNECT_CFM
                if matches!(&control.operation, ControlOperation::Connect(connect)
                    if connect.phase == ConnectPhase::Resetting) =>
            {
                crate::lmac::require_empty(SM_DISCONNECT_CFM, &result)?;
                control.commands.pop_front();
                let ControlOperation::Connect(connect) = &mut control.operation else {
                    return Err(AicError::CompletionMismatch);
                };
                connect.phase = ConnectPhase::AwaitConfirmation;
                log::info!("[wifi] stale station association cleared before connect");
            }
            SM_CONNECT_CFM => {
                if result.len() != 1 {
                    return Err(AicError::MalformedResponse);
                }
                crate::lmac::require_status_ok(SM_CONNECT_CFM, &result)?;
                let ControlOperation::Connect(connect) = &mut control.operation else {
                    return Err(AicError::CompletionMismatch);
                };
                if connect.phase != ConnectPhase::AwaitConfirmation {
                    return Err(AicError::CompletionMismatch);
                }
                connect.phase = ConnectPhase::AwaitIndication;
                control.commands.pop_front();
            }
            ME_SET_CONTROL_PORT_CFM => {
                crate::lmac::require_empty(ME_SET_CONTROL_PORT_CFM, &result)?;
                let ControlOperation::Connect(connect) = &mut control.operation else {
                    return Err(AicError::CompletionMismatch);
                };
                if connect.phase != ConnectPhase::AwaitControlPort {
                    return Err(AicError::CompletionMismatch);
                }
                control.commands.pop_front();
                open_control_port = true;
                finish = true;
                log::info!("[wifi] WPA2 keys installed and control port enabled");
            }
            MM_KEY_ADD_CFM => {
                crate::lmac::parse_key_add_confirmation(&result)?;
                control.commands.pop_front();
                m4 = control.accept_key_confirmation()?;
            }
            SM_DISCONNECT_CFM => {
                crate::lmac::require_empty(SM_DISCONNECT_CFM, &result)?;
                control.commands.pop_front();
                self.data.link.clear_peer();
                finish = true;
            }
            _ => {
                control.commands.pop_front();
                finish = control.commands.is_empty();
            }
        }
        if open_control_port {
            self.data.link.open_control_port()?;
        }
        if let Some(frame) = m4 {
            self.queue_internal_eapol(super::owner::InternalTxKind::M4, frame)?;
        }
        if finish {
            self.lifecycle.control = None;
            self.data.events.push_back(AicEvent::ControlComplete);
        }
        Ok(())
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
        let phase = match self.mailbox_flow_policy() {
            MailboxFlowPolicy::Direct => MailboxPhase::Write,
            MailboxFlowPolicy::CreditGated => MailboxPhase::Flow,
        };
        self.lifecycle.mailbox = Some(MailboxState {
            frame: debug_command_frame(message_id, payload, self.transport_uses_header_crc()),
            request: debug_mailbox_request(message_id, payload),
            expected_message_id: message_id + 1,
            phase,
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
        let phase = match self.mailbox_flow_policy() {
            MailboxFlowPolicy::Direct => MailboxPhase::Write,
            MailboxFlowPolicy::CreditGated => MailboxPhase::Flow,
        };
        self.lifecycle.mailbox = Some(MailboxState {
            frame: command_frame(
                message_id,
                destination,
                payload,
                self.transport_uses_header_crc(),
            ),
            request: MailboxRequest::Lmac { message_id },
            expected_message_id,
            phase,
            deadline: now.after(MAILBOX_TIMEOUT),
            retry_at: None,
            flow_retries: 0,
            result: None,
        });
    }
}

fn mailbox_timeout(mailbox: &MailboxState) -> AicError {
    AicError::MailboxTimeout {
        request: mailbox.request,
        expected_message_id: mailbox.expected_message_id,
        phase: mailbox.phase.into(),
    }
}

fn debug_mailbox_request(message_id: u16, payload: &[u8]) -> MailboxRequest {
    let address = payload
        .get(..4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four-byte slice")));
    match (message_id, address) {
        (DBG_MEM_READ_REQ, Some(address)) => MailboxRequest::DebugMemoryRead { address },
        (DBG_MEM_WRITE_REQ, Some(address)) => MailboxRequest::DebugMemoryWrite { address },
        (DBG_MEM_BLOCK_WRITE_REQ, Some(address)) => {
            let Some(length) = payload
                .get(4..8)
                .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four-byte slice")))
            else {
                return MailboxRequest::Debug { message_id };
            };
            MailboxRequest::DebugMemoryBlockWrite { address, length }
        }
        (DBG_MEM_MASK_WRITE_REQ, Some(address)) => MailboxRequest::DebugMemoryMaskWrite { address },
        (DBG_START_APP_REQ, Some(address)) => MailboxRequest::StartApplication { address },
        _ => MailboxRequest::Debug { message_id },
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::{common::ChipVariant, lmac::SM_DISCONNECT_REQ, protocol::DBG_MEM_READ_REQ};

    fn complete(request: &SdioRequest, response: SdioResponse, now: MonotonicTime) -> AicInput {
        AicInput {
            now,
            event: Some(AicInputEvent::Sdio(SdioCompletion {
                request_id: request.id,
                result: Ok(response),
            })),
        }
    }

    fn ready_device_with_control(request: ControlRequest, result: Vec<u8>) -> AicDevice {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Ready;
        device.data.link.install_mac([2, 0, 0, 0, 0, 1]).unwrap();
        device.data.link.install_interface(2).unwrap();
        let mut control =
            super::super::control::build(request, [2, 0, 0, 0, 0, 1], Some(2)).unwrap();
        // These tests exercise the connect confirmation boundary directly;
        // the warm-reset prelude has its own sequence test below.
        assert_eq!(
            control
                .commands
                .pop_front()
                .map(|command| command.message_id),
            Some(SM_DISCONNECT_REQ)
        );
        if let super::control::ControlOperation::Connect(connect) = &mut control.operation {
            connect.phase = super::control::ConnectPhase::AwaitConfirmation;
        }
        device.lifecycle.control = Some(control);
        let expected_message_id = device
            .lifecycle
            .control
            .as_ref()
            .and_then(|control| control.commands.front())
            .map(|command| command.expected_message_id)
            .unwrap();
        device.lifecycle.mailbox = Some(MailboxState {
            frame: Vec::new(),
            request: MailboxRequest::Lmac {
                message_id: expected_message_id.wrapping_sub(1),
            },
            expected_message_id,
            phase: MailboxPhase::Complete,
            deadline: MonotonicTime::from_nanos(1),
            retry_at: None,
            flow_retries: 0,
            result: Some(result),
        });
        device
    }

    #[test]
    fn dc_firmware_mailbox_bypasses_data_fifo_flow_credits() {
        let now = MonotonicTime::from_nanos(0);
        let mut dc = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        dc.begin_debug_mailbox(DBG_MEM_READ_REQ, &[0; 4], now);
        assert!(matches!(
            dc.lifecycle.mailbox.as_ref().map(|mailbox| mailbox.phase),
            Some(MailboxPhase::Write)
        ));

        let mut d80 = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        d80.begin_debug_mailbox(DBG_MEM_READ_REQ, &[0; 4], now);
        assert!(matches!(
            d80.lifecycle.mailbox.as_ref().map(|mailbox| mailbox.phase),
            Some(MailboxPhase::Flow)
        ));
    }

    #[test]
    fn dc_mailbox_confirmation_waits_for_card_interrupt_and_scans_both_functions() {
        let start = MonotonicTime::from_nanos(0);
        let settled = MonotonicTime::from_nanos(2_000_000);
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Ready;
        device.begin_debug_mailbox(DBG_MEM_READ_REQ, &[0; 4], start);

        let AicAction::SubmitSdio(write) = device.advance(AicInput::tick(start)) else {
            panic!("expected command-function write")
        };
        assert!(matches!(
            write.kind,
            SdioRequestKind::Write { function, .. } if function.get() == 2
        ));
        let confirmation_wait = device.advance(complete(&write, SdioResponse::Unit, start));
        assert!(
            !matches!(confirmation_wait, AicAction::RetryAt(_)),
            "a mailbox confirmation needs a card interrupt, not a timer-only wakeup"
        );

        let AicAction::SubmitSdio(command_count) = device.advance(AicInput {
            now: settled,
            event: Some(AicInputEvent::Irq(IrqSnapshot {
                sequence: 1,
                card_interrupt: true,
                ..IrqSnapshot::default()
            })),
        }) else {
            panic!("expected command-function receive count")
        };
        assert!(matches!(
            command_count.kind,
            SdioRequestKind::ReadByte { function, .. } if function.get() == 2
        ));

        let AicAction::SubmitSdio(data_count) =
            device.advance(complete(&command_count, SdioResponse::Byte(0), settled))
        else {
            panic!("expected data-function receive count after an empty command function")
        };
        assert!(matches!(
            data_count.kind,
            SdioRequestKind::ReadByte { function, .. } if function.get() == 1
        ));
    }

    #[test]
    fn startup_mailbox_does_not_queue_unbounded_receive_rescans() {
        let now = MonotonicTime::from_nanos(0);
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.begin_debug_mailbox(DBG_MEM_READ_REQ, &[0; 4], now);
        device.lifecycle.mailbox.as_mut().unwrap().phase = MailboxPhase::Confirmation;

        let AicAction::SubmitSdio(_) = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::Irq(IrqSnapshot {
                sequence: 1,
                card_interrupt: true,
                ..IrqSnapshot::default()
            })),
        }) else {
            panic!("expected first bounded receive scan request")
        };
        assert!(device.io.receive.active);

        let _ = device.advance(AicInput {
            now,
            event: Some(AicInputEvent::Irq(IrqSnapshot {
                sequence: 2,
                card_interrupt: true,
                ..IrqSnapshot::default()
            })),
        });
        assert!(device.io.receive.active);
    }

    #[test]
    fn startup_card_interrupts_do_not_preempt_function_setup() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Starting;

        device.request_receive_scan();

        assert!(!device.io.receive.active);
    }

    #[test]
    fn ready_mailbox_is_serviced_before_a_repeated_card_receive_scan() {
        let now = MonotonicTime::from_nanos(0);
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Ready;
        device.request_receive_scan();
        device.begin_debug_mailbox(DBG_MEM_READ_REQ, &[0; 4], now);

        let AicAction::SubmitSdio(request) = device.advance(AicInput::tick(now)) else {
            panic!("expected mailbox write to take priority over the receive scan")
        };
        assert!(
            matches!(request.kind, SdioRequestKind::Write { function, .. } if function.get() == 2)
        );
    }

    #[test]
    fn out_of_order_confirmation_is_rejected_before_startup_state_changes() {
        let now = MonotonicTime::from_nanos(0);
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.begin_debug_mailbox(DBG_MEM_READ_REQ, &[0; 4], now);
        device.lifecycle.mailbox.as_mut().unwrap().phase = MailboxPhase::Confirmation;

        assert_eq!(
            device.accept_mailbox_confirmation(DBG_MEM_READ_REQ + 3, vec![]),
            Err(AicError::UnexpectedConfirmation {
                expected_message_id: DBG_MEM_READ_REQ + 1,
                actual_message_id: DBG_MEM_READ_REQ + 3,
            })
        );
        assert!(matches!(
            device
                .lifecycle
                .mailbox
                .as_ref()
                .map(|mailbox| mailbox.phase),
            Some(MailboxPhase::Confirmation)
        ));
    }

    #[test]
    fn received_confirmation_wins_over_the_same_deadline_observation() {
        let start = MonotonicTime::from_nanos(0);
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.begin_debug_mailbox(DBG_MEM_READ_REQ, &[0; 4], start);
        let deadline = device.lifecycle.mailbox.as_ref().unwrap().deadline;
        let mailbox = device.lifecycle.mailbox.as_mut().unwrap();
        mailbox.phase = MailboxPhase::Complete;
        mailbox.result = Some(vec![0; 8]);

        assert!(!device.mailbox_timed_out(deadline));
    }

    #[test]
    fn connect_confirmation_does_not_complete_before_connect_indication() {
        let mut device = ready_device_with_control(
            ControlRequest::Connect {
                ssid: b"network".to_vec(),
                pmk: None,
                entropy: None,
            },
            vec![0],
        );

        assert_eq!(device.complete_mailbox(), Ok(()));
        assert!(device.lifecycle.control.is_some());
        assert!(device.data.events.is_empty());
    }

    #[test]
    fn nonzero_connect_confirmation_status_is_rejected() {
        let mut device = ready_device_with_control(
            ControlRequest::Connect {
                ssid: b"network".to_vec(),
                pmk: None,
                entropy: None,
            },
            vec![7],
        );

        assert_eq!(
            device.complete_mailbox(),
            Err(AicError::FirmwareRejected {
                message_id: 0x1801,
                status: 7,
            })
        );
    }
}
