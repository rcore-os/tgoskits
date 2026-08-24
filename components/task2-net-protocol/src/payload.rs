//! Typed payloads carried by task-2 message kinds.

use thiserror::Error;

const CONTROL_PAYLOAD_LEN: usize = 12;
const STATUS_PAYLOAD_LEN: usize = 12;
const HEARTBEAT_PAYLOAD_LEN: usize = 8;

/// Control action requested by the controller Guest.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlAction {
    /// Sets a managed output to a bounded integer value.
    SetOutput = 1,
    /// Requests a transition into a safe stopped state.
    Stop      = 2,
    /// Requests recovery from the safe state.
    Reset     = 3,
}

impl TryFrom<u8> for ControlAction {
    type Error = PayloadError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::SetOutput),
            2 => Ok(Self::Stop),
            3 => Ok(Self::Reset),
            _ => Err(PayloadError::UnknownControlAction(value)),
        }
    }
}

/// A validated control command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlMessage {
    action: ControlAction,
    value: i32,
    request_id: u32,
}

impl ControlMessage {
    /// Maximum value accepted by [`ControlAction::SetOutput`].
    pub const MAX_OUTPUT_VALUE: i32 = 1000;

    /// Creates a control command and validates action-specific arguments.
    ///
    /// # Errors
    ///
    /// Returns [`PayloadError::InvalidControlValue`] when `value` is not valid
    /// for the selected action.
    pub const fn new(
        action: ControlAction,
        value: i32,
        request_id: u32,
    ) -> Result<Self, PayloadError> {
        match action {
            ControlAction::SetOutput if value < 0 || value > Self::MAX_OUTPUT_VALUE => {
                Err(PayloadError::InvalidControlValue)
            }
            ControlAction::Stop | ControlAction::Reset if value != 0 => {
                Err(PayloadError::InvalidControlValue)
            }
            _ => Ok(Self {
                action,
                value,
                request_id,
            }),
        }
    }

    /// Returns the requested action.
    pub const fn action(self) -> ControlAction {
        self.action
    }

    /// Returns the action argument.
    pub const fn value(self) -> i32 {
        self.value
    }

    /// Returns the caller-provided idempotency key.
    pub const fn request_id(self) -> u32 {
        self.request_id
    }

    /// Encodes the command into the fixed twelve-byte wire payload.
    pub fn encode(self, output: &mut [u8]) -> Result<usize, PayloadError> {
        if output.len() < CONTROL_PAYLOAD_LEN {
            return Err(PayloadError::OutputTooSmall {
                needed: CONTROL_PAYLOAD_LEN,
                available: output.len(),
            });
        }
        output[..CONTROL_PAYLOAD_LEN].fill(0);
        output[0] = self.action as u8;
        output[4..8].copy_from_slice(&self.value.to_be_bytes());
        output[8..12].copy_from_slice(&self.request_id.to_be_bytes());
        Ok(CONTROL_PAYLOAD_LEN)
    }

    /// Decodes and validates a control payload.
    pub fn decode(payload: &[u8]) -> Result<Self, PayloadError> {
        if payload.len() != CONTROL_PAYLOAD_LEN {
            return Err(PayloadError::InvalidLength {
                expected: CONTROL_PAYLOAD_LEN,
                actual: payload.len(),
            });
        }
        if payload[1..4] != [0, 0, 0] {
            return Err(PayloadError::ReservedBitsNonZero);
        }
        let action = ControlAction::try_from(payload[0])?;
        let value = i32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
        let request_id = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
        Self::new(action, value, request_id)
    }
}

/// Managed Guest state reported to the controller.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusState {
    /// Normal operation.
    Active  = 1,
    /// Deliberate stop requested by the controller.
    Stopped = 2,
    /// Safety state entered after timeout or protocol failure.
    Safe    = 3,
}

impl TryFrom<u8> for StatusState {
    type Error = PayloadError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Active),
            2 => Ok(Self::Stopped),
            3 => Ok(Self::Safe),
            _ => Err(PayloadError::UnknownStatusState(value)),
        }
    }
}

/// A validated state report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusMessage {
    state: StatusState,
    flags: u8,
    value: i32,
    last_control_request: u32,
}

impl StatusMessage {
    /// Creates a status report.
    pub const fn new(
        state: StatusState,
        flags: u8,
        value: i32,
        last_control_request: u32,
    ) -> Result<Self, PayloadError> {
        if flags & !0x03 != 0 {
            return Err(PayloadError::ReservedBitsNonZero);
        }
        Ok(Self {
            state,
            flags,
            value,
            last_control_request,
        })
    }

    /// Returns the state.
    pub const fn state(self) -> StatusState {
        self.state
    }

    /// Returns state flags.
    pub const fn flags(self) -> u8 {
        self.flags
    }

    /// Returns the state value.
    pub const fn value(self) -> i32 {
        self.value
    }

    /// Returns the last applied control request id.
    pub const fn last_control_request(self) -> u32 {
        self.last_control_request
    }

    /// Encodes the state report.
    pub fn encode(self, output: &mut [u8]) -> Result<usize, PayloadError> {
        if output.len() < STATUS_PAYLOAD_LEN {
            return Err(PayloadError::OutputTooSmall {
                needed: STATUS_PAYLOAD_LEN,
                available: output.len(),
            });
        }
        output[..STATUS_PAYLOAD_LEN].fill(0);
        output[0] = self.state as u8;
        output[1] = self.flags;
        output[4..8].copy_from_slice(&self.value.to_be_bytes());
        output[8..12].copy_from_slice(&self.last_control_request.to_be_bytes());
        Ok(STATUS_PAYLOAD_LEN)
    }

    /// Decodes and validates a state report.
    pub fn decode(payload: &[u8]) -> Result<Self, PayloadError> {
        if payload.len() != STATUS_PAYLOAD_LEN {
            return Err(PayloadError::InvalidLength {
                expected: STATUS_PAYLOAD_LEN,
                actual: payload.len(),
            });
        }
        if payload[2..4] != [0, 0] {
            return Err(PayloadError::ReservedBitsNonZero);
        }
        let state = StatusState::try_from(payload[0])?;
        let value = i32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
        let request_id = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
        Self::new(state, payload[1], value, request_id)
    }
}

/// Heartbeat payload containing the sender's monotonic uptime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeartbeatMessage {
    uptime_ms: u64,
}

impl HeartbeatMessage {
    /// Creates a heartbeat payload.
    pub const fn new(uptime_ms: u64) -> Self {
        Self { uptime_ms }
    }

    /// Returns the sender uptime.
    pub const fn uptime_ms(self) -> u64 {
        self.uptime_ms
    }

    /// Encodes the heartbeat payload.
    pub fn encode(self, output: &mut [u8]) -> Result<usize, PayloadError> {
        if output.len() < HEARTBEAT_PAYLOAD_LEN {
            return Err(PayloadError::OutputTooSmall {
                needed: HEARTBEAT_PAYLOAD_LEN,
                available: output.len(),
            });
        }
        output[..HEARTBEAT_PAYLOAD_LEN].copy_from_slice(&self.uptime_ms.to_be_bytes());
        Ok(HEARTBEAT_PAYLOAD_LEN)
    }

    /// Decodes a heartbeat payload.
    pub fn decode(payload: &[u8]) -> Result<Self, PayloadError> {
        if payload.len() != HEARTBEAT_PAYLOAD_LEN {
            return Err(PayloadError::InvalidLength {
                expected: HEARTBEAT_PAYLOAD_LEN,
                actual: payload.len(),
            });
        }
        Ok(Self::new(u64::from_be_bytes([
            payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
            payload[7],
        ])))
    }
}

/// Error returned when typed payload validation fails.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PayloadError {
    /// Payload has the wrong fixed size.
    #[error("payload length is {actual}, expected {expected}")]
    InvalidLength {
        /// Expected byte count.
        expected: usize,
        /// Actual byte count.
        actual: usize,
    },
    /// A reserved field was not zero.
    #[error("reserved payload bits are nonzero")]
    ReservedBitsNonZero,
    /// Unknown control action byte.
    #[error("unknown control action {0}")]
    UnknownControlAction(u8),
    /// Unknown status state byte.
    #[error("unknown status state {0}")]
    UnknownStatusState(u8),
    /// Action argument violates its range or zero-value rule.
    #[error("invalid control value")]
    InvalidControlValue,
    /// Output buffer cannot hold the fixed payload.
    #[error("payload output buffer has {available} bytes but needs {needed}")]
    OutputTooSmall {
        /// Required output size.
        needed: usize,
        /// Available output size.
        available: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_payload_round_trips() {
        let message = ControlMessage::new(ControlAction::SetOutput, 42, 7).unwrap();
        let mut payload = [0; CONTROL_PAYLOAD_LEN];
        let len = message.encode(&mut payload).unwrap();

        assert_eq!(ControlMessage::decode(&payload[..len]).unwrap(), message);
    }

    #[test]
    fn control_payload_rejects_nonzero_reserved_bits() {
        let mut payload = [0; CONTROL_PAYLOAD_LEN];
        payload[1] = 1;

        assert_eq!(
            ControlMessage::decode(&payload),
            Err(PayloadError::ReservedBitsNonZero)
        );
    }

    #[test]
    fn stop_rejects_a_nonzero_value() {
        assert_eq!(
            ControlMessage::new(ControlAction::Stop, 1, 1),
            Err(PayloadError::InvalidControlValue)
        );
    }

    #[test]
    fn status_payload_round_trips() {
        let message = StatusMessage::new(StatusState::Safe, 2, -3, 11).unwrap();
        let mut payload = [0; STATUS_PAYLOAD_LEN];
        let len = message.encode(&mut payload).unwrap();

        assert_eq!(StatusMessage::decode(&payload[..len]).unwrap(), message);
    }

    #[test]
    fn heartbeat_payload_round_trips() {
        let message = HeartbeatMessage::new(1234);
        let mut payload = [0; HEARTBEAT_PAYLOAD_LEN];
        let len = message.encode(&mut payload).unwrap();

        assert_eq!(HeartbeatMessage::decode(&payload[..len]).unwrap(), message);
    }
}
