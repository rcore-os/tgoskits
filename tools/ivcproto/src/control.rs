//! Fixed-size application payloads shared with the RTOS implementation.

use thiserror::Error;

use crate::wire::{ErrorCode, MessageType};

pub const CONTROL_PAYLOAD_LEN: usize = 12;
pub const STATUS_PAYLOAD_LEN: usize = 20;
pub const ACK_PAYLOAD_LEN: usize = 12;
pub const ERROR_PAYLOAD_LEN: usize = 8;

pub const MIN_TEMPERATURE_MILLI_C: i32 = -40_000;
pub const MAX_TEMPERATURE_MILLI_C: i32 = 150_000;
pub const MAX_ACTUATOR_PERMILLE: u16 = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ControlOperation {
    SetActuator    = 1,
    EnterSafeState = 2,
    Heartbeat      = 3,
}

impl TryFrom<u8> for ControlOperation {
    type Error = PayloadError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::SetActuator),
            2 => Ok(Self::EnterSafeState),
            3 => Ok(Self::Heartbeat),
            other => Err(PayloadError::UnsupportedControlOperation(other)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ControlMode {
    Safe        = 0,
    ManualFixed = 1,
    Neural      = 2,
}

impl TryFrom<u8> for ControlMode {
    type Error = PayloadError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Safe),
            1 => Ok(Self::ManualFixed),
            2 => Ok(Self::Neural),
            other => Err(PayloadError::UnsupportedControlMode(other)),
        }
    }
}

/// Command computed in the Linux/StarryOS guest and applied exactly once by RTOS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlCommand {
    pub operation: ControlOperation,
    pub mode: ControlMode,
    pub actuator_permille: u16,
    pub setpoint_milli_c: i32,
    pub sample_id: u32,
}

impl ControlCommand {
    pub fn encode(self) -> Result<[u8; CONTROL_PAYLOAD_LEN], PayloadError> {
        self.validate()?;
        let mut bytes = [0u8; CONTROL_PAYLOAD_LEN];
        bytes[0] = self.operation as u8;
        bytes[1] = self.mode as u8;
        bytes[2..4].copy_from_slice(&self.actuator_permille.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.setpoint_milli_c.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.sample_id.to_le_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PayloadError> {
        expect_len(bytes, CONTROL_PAYLOAD_LEN)?;
        let command = Self {
            operation: ControlOperation::try_from(bytes[0])?,
            mode: ControlMode::try_from(bytes[1])?,
            actuator_permille: u16::from_le_bytes([bytes[2], bytes[3]]),
            setpoint_milli_c: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            sample_id: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        };
        command.validate()?;
        Ok(command)
    }

    pub fn validate(self) -> Result<(), PayloadError> {
        validate_temperature(self.setpoint_milli_c)?;
        if self.actuator_permille > MAX_ACTUATOR_PERMILLE {
            return Err(PayloadError::ActuatorOutOfRange(self.actuator_permille));
        }
        if self.operation == ControlOperation::EnterSafeState && self.mode != ControlMode::Safe {
            return Err(PayloadError::UnsafeStateMode(self.mode));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum StatusState {
    Ready        = 1,
    Applied      = 2,
    SafeFallback = 3,
    Fault        = 4,
}

impl TryFrom<u8> for StatusState {
    type Error = PayloadError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Ready),
            2 => Ok(Self::Applied),
            3 => Ok(Self::SafeFallback),
            4 => Ok(Self::Fault),
            other => Err(PayloadError::UnsupportedStatusState(other)),
        }
    }
}

/// RTOS state returned after applying a command or entering the safe fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusReport {
    pub state: StatusState,
    pub active_mode: ControlMode,
    pub actuator_permille: u16,
    pub measured_milli_c: i32,
    pub setpoint_milli_c: i32,
    pub applied_sequence: u32,
    pub fault: ErrorCode,
}

impl StatusReport {
    pub fn encode(self) -> Result<[u8; STATUS_PAYLOAD_LEN], PayloadError> {
        self.validate()?;
        let mut bytes = [0u8; STATUS_PAYLOAD_LEN];
        bytes[0] = self.state as u8;
        bytes[1] = self.active_mode as u8;
        bytes[2..4].copy_from_slice(&self.actuator_permille.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.measured_milli_c.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.setpoint_milli_c.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.applied_sequence.to_le_bytes());
        bytes[16..18].copy_from_slice(&(self.fault as u16).to_le_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PayloadError> {
        expect_len(bytes, STATUS_PAYLOAD_LEN)?;
        if bytes[18] != 0 || bytes[19] != 0 {
            return Err(PayloadError::NonzeroReservedField);
        }
        let status = Self {
            state: StatusState::try_from(bytes[0])?,
            active_mode: ControlMode::try_from(bytes[1])?,
            actuator_permille: u16::from_le_bytes([bytes[2], bytes[3]]),
            measured_milli_c: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            setpoint_milli_c: i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            applied_sequence: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            fault: ErrorCode::try_from(u16::from_le_bytes([bytes[16], bytes[17]])).map_err(
                |_| PayloadError::UnsupportedErrorCode(u16::from_le_bytes([bytes[16], bytes[17]])),
            )?,
        };
        status.validate()?;
        Ok(status)
    }

    fn validate(self) -> Result<(), PayloadError> {
        validate_temperature(self.measured_milli_c)?;
        validate_temperature(self.setpoint_milli_c)?;
        if self.actuator_permille > MAX_ACTUATOR_PERMILLE {
            return Err(PayloadError::ActuatorOutOfRange(self.actuator_permille));
        }
        match self.state {
            StatusState::Fault if self.fault == ErrorCode::None => {
                Err(PayloadError::FaultStatusWithoutError)
            }
            StatusState::Fault | StatusState::SafeFallback => Ok(()),
            _ if self.fault != ErrorCode::None => Err(PayloadError::UnexpectedStatusError),
            _ => Ok(()),
        }
    }
}

/// Selective acknowledgement used by the bounded receive window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AckPayload {
    pub acknowledged_sequence: u32,
    pub next_expected_sequence: u32,
    pub received_mask: u32,
}

impl AckPayload {
    pub fn encode(self) -> [u8; ACK_PAYLOAD_LEN] {
        let mut bytes = [0u8; ACK_PAYLOAD_LEN];
        bytes[..4].copy_from_slice(&self.acknowledged_sequence.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.next_expected_sequence.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.received_mask.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PayloadError> {
        expect_len(bytes, ACK_PAYLOAD_LEN)?;
        Ok(Self {
            acknowledged_sequence: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            next_expected_sequence: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            received_mask: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        })
    }
}

/// Context attached to a protocol error response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ErrorReport {
    pub offending_type: MessageType,
    pub offending_sequence: u32,
}

impl ErrorReport {
    pub fn encode(self) -> [u8; ERROR_PAYLOAD_LEN] {
        let mut bytes = [0u8; ERROR_PAYLOAD_LEN];
        bytes[0] = self.offending_type as u8;
        bytes[4..8].copy_from_slice(&self.offending_sequence.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PayloadError> {
        expect_len(bytes, ERROR_PAYLOAD_LEN)?;
        if bytes[1..4] != [0, 0, 0] {
            return Err(PayloadError::NonzeroReservedField);
        }
        Ok(Self {
            offending_type: MessageType::try_from(bytes[0])
                .map_err(|_| PayloadError::UnsupportedMessageType(bytes[0]))?,
            offending_sequence: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PayloadError {
    #[error("payload length must be {expected} bytes, received {actual}")]
    InvalidLength { expected: usize, actual: usize },
    #[error("unsupported control operation {0}")]
    UnsupportedControlOperation(u8),
    #[error("unsupported control mode {0}")]
    UnsupportedControlMode(u8),
    #[error("unsupported status state {0}")]
    UnsupportedStatusState(u8),
    #[error("unsupported message type {0}")]
    UnsupportedMessageType(u8),
    #[error("unsupported error code {0}")]
    UnsupportedErrorCode(u16),
    #[error("actuator command {0} is outside 0..=1000 permille")]
    ActuatorOutOfRange(u16),
    #[error("temperature {0} mC is outside the supported sensor range")]
    TemperatureOutOfRange(i32),
    #[error("safe-state command must use safe control mode, got {0:?}")]
    UnsafeStateMode(ControlMode),
    #[error("fault status must carry a nonzero error code")]
    FaultStatusWithoutError,
    #[error("non-fault status carries an error code")]
    UnexpectedStatusError,
    #[error("reserved payload field is nonzero")]
    NonzeroReservedField,
}

fn expect_len(bytes: &[u8], expected: usize) -> Result<(), PayloadError> {
    if bytes.len() != expected {
        return Err(PayloadError::InvalidLength {
            expected,
            actual: bytes.len(),
        });
    }
    Ok(())
}

fn validate_temperature(temperature_milli_c: i32) -> Result<(), PayloadError> {
    if !(MIN_TEMPERATURE_MILLI_C..=MAX_TEMPERATURE_MILLI_C).contains(&temperature_milli_c) {
        return Err(PayloadError::TemperatureOutOfRange(temperature_milli_c));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_command_has_c_compatible_golden_bytes() {
        let command = ControlCommand {
            operation: ControlOperation::SetActuator,
            mode: ControlMode::Neural,
            actuator_permille: 625,
            setpoint_milli_c: 55_000,
            sample_id: 0x0102_0304,
        };
        assert_eq!(
            command.encode().unwrap(),
            [1, 2, 0x71, 0x02, 0xd8, 0xd6, 0, 0, 4, 3, 2, 1]
        );
        assert_eq!(
            ControlCommand::decode(&command.encode().unwrap()).unwrap(),
            command
        );
    }

    #[test]
    fn out_of_range_actuator_is_rejected() {
        let command = ControlCommand {
            operation: ControlOperation::SetActuator,
            mode: ControlMode::Neural,
            actuator_permille: 1_001,
            setpoint_milli_c: 55_000,
            sample_id: 1,
        };
        assert_eq!(
            command.encode(),
            Err(PayloadError::ActuatorOutOfRange(1_001))
        );
    }

    #[test]
    fn malformed_status_reserved_bytes_are_rejected() {
        let mut bytes = StatusReport {
            state: StatusState::Ready,
            active_mode: ControlMode::Safe,
            actuator_permille: 0,
            measured_milli_c: 20_000,
            setpoint_milli_c: 50_000,
            applied_sequence: 0,
            fault: ErrorCode::None,
        }
        .encode()
        .unwrap();
        bytes[18] = 1;
        assert_eq!(
            StatusReport::decode(&bytes),
            Err(PayloadError::NonzeroReservedField)
        );
    }
}
