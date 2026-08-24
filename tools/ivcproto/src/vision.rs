//! Typed payloads and fail-safe RTOS state for the visual sorting demo.

use thiserror::Error;

use crate::wire::ErrorCode;

/// Version of the visual sorting payloads, independent of the IVC frame version.
pub const VISION_PAYLOAD_VERSION: u8 = 1;
/// Encoded length of [`VisionDecision`].
pub const VISION_DECISION_PAYLOAD_LEN: usize = 44;
/// Encoded length of [`ActuatorStatus`].
pub const ACTUATOR_STATUS_PAYLOAD_LEN: usize = 32;
/// Largest accepted lifetime for a decision.
pub const MAX_DECISION_TTL_US: u32 = 5_000_000;
/// Class marker used when no detection is present.
pub const NO_DETECTION_CLASS_ID: u16 = u16::MAX;

/// Requested or applied virtual diverter action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum VisionAction {
    Hold          = 0,
    SortLeft      = 1,
    SortRight     = 2,
    EmergencyStop = 3,
}

impl TryFrom<u8> for VisionAction {
    type Error = VisionError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Hold),
            1 => Ok(Self::SortLeft),
            2 => Ok(Self::SortRight),
            3 => Ok(Self::EmergencyStop),
            other => Err(VisionError::UnsupportedAction(other)),
        }
    }
}

impl VisionAction {
    const fn is_safe(self) -> bool {
        matches!(self, Self::Hold | Self::EmergencyStop)
    }
}

/// Pixel-space detection bounds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BoundingBox {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl BoundingBox {
    const fn is_empty(self) -> bool {
        self.left == 0 && self.top == 0 && self.right == 0 && self.bottom == 0
    }

    const fn is_ordered(self) -> bool {
        self.left < self.right && self.top < self.bottom
    }
}

/// A time-bounded action derived from one StarryOS inference result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VisionDecision {
    pub requested_action: VisionAction,
    pub safe_action: VisionAction,
    pub detection_present: bool,
    pub frame_id: u32,
    pub captured_at_us: u64,
    pub inference_finished_at_us: u64,
    pub ttl_us: u32,
    pub class_id: u16,
    pub confidence_q10000: u16,
    pub region_id: u16,
    pub bounding_box: BoundingBox,
}

impl VisionDecision {
    /// Encodes a validated decision into the fixed little-endian wire layout.
    pub fn encode(self) -> Result<[u8; VISION_DECISION_PAYLOAD_LEN], VisionError> {
        self.validate()?;
        let mut bytes = [0u8; VISION_DECISION_PAYLOAD_LEN];
        bytes[0] = VISION_PAYLOAD_VERSION;
        bytes[1] = self.requested_action as u8;
        bytes[2] = self.safe_action as u8;
        bytes[3] = u8::from(self.detection_present);
        bytes[4..8].copy_from_slice(&self.frame_id.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.captured_at_us.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.inference_finished_at_us.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.ttl_us.to_le_bytes());
        bytes[28..30].copy_from_slice(&self.class_id.to_le_bytes());
        bytes[30..32].copy_from_slice(&self.confidence_q10000.to_le_bytes());
        bytes[32..34].copy_from_slice(&self.region_id.to_le_bytes());
        bytes[36..38].copy_from_slice(&self.bounding_box.left.to_le_bytes());
        bytes[38..40].copy_from_slice(&self.bounding_box.top.to_le_bytes());
        bytes[40..42].copy_from_slice(&self.bounding_box.right.to_le_bytes());
        bytes[42..44].copy_from_slice(&self.bounding_box.bottom.to_le_bytes());
        Ok(bytes)
    }

    /// Decodes and validates one complete decision payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, VisionError> {
        expect_len(bytes, VISION_DECISION_PAYLOAD_LEN)?;
        if bytes[0] != VISION_PAYLOAD_VERSION {
            return Err(VisionError::UnsupportedPayloadVersion(bytes[0]));
        }
        if bytes[3] & !1 != 0 {
            return Err(VisionError::UnsupportedFlags(bytes[3]));
        }
        if bytes[34..36] != [0, 0] {
            return Err(VisionError::NonzeroReservedField);
        }
        let decision = Self {
            requested_action: VisionAction::try_from(bytes[1])?,
            safe_action: VisionAction::try_from(bytes[2])?,
            detection_present: bytes[3] != 0,
            frame_id: read_u32(bytes, 4),
            captured_at_us: read_u64(bytes, 8),
            inference_finished_at_us: read_u64(bytes, 16),
            ttl_us: read_u32(bytes, 24),
            class_id: read_u16(bytes, 28),
            confidence_q10000: read_u16(bytes, 30),
            region_id: read_u16(bytes, 32),
            bounding_box: BoundingBox {
                left: read_u16(bytes, 36),
                top: read_u16(bytes, 38),
                right: read_u16(bytes, 40),
                bottom: read_u16(bytes, 42),
            },
        };
        decision.validate()?;
        Ok(decision)
    }

    /// Checks the safety and internal-consistency invariants of a decision.
    pub fn validate(self) -> Result<(), VisionError> {
        if !self.safe_action.is_safe() {
            return Err(VisionError::UnsafeFallbackAction);
        }
        if self.frame_id == 0 {
            return Err(VisionError::ZeroFrameId);
        }
        if self.captured_at_us > self.inference_finished_at_us {
            return Err(VisionError::InvalidTimestampOrder);
        }
        if !(1..=MAX_DECISION_TTL_US).contains(&self.ttl_us) {
            return Err(VisionError::InvalidTtl(self.ttl_us));
        }
        if self.confidence_q10000 > 10_000 {
            return Err(VisionError::InvalidConfidence(self.confidence_q10000));
        }
        let detected_fields_valid = self.class_id != NO_DETECTION_CLASS_ID
            && self.confidence_q10000 != 0
            && self.bounding_box.is_ordered();
        let empty_fields_valid = self.requested_action == VisionAction::Hold
            && self.class_id == NO_DETECTION_CLASS_ID
            && self.confidence_q10000 == 0
            && self.region_id == 0
            && self.bounding_box.is_empty();
        if (self.detection_present && !detected_fields_valid)
            || (!self.detection_present && !empty_fields_valid)
        {
            return Err(VisionError::InconsistentDetection);
        }
        Ok(())
    }
}

/// RTOS outcome returned for an accepted visual decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ActuatorState {
    Applied      = 1,
    SafeFallback = 2,
    Rejected     = 3,
    Fault        = 4,
}

impl TryFrom<u8> for ActuatorState {
    type Error = VisionError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Applied),
            2 => Ok(Self::SafeFallback),
            3 => Ok(Self::Rejected),
            4 => Ok(Self::Fault),
            other => Err(VisionError::UnsupportedActuatorState(other)),
        }
    }
}

/// Observable RTOS action, with sender-age and RTOS-local latency kept separate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActuatorStatus {
    pub state: ActuatorState,
    pub requested_action: VisionAction,
    pub actual_action: VisionAction,
    pub frame_id: u32,
    pub applied_sequence: u32,
    pub decision_age_at_send_us: u32,
    pub local_apply_latency_us: u32,
    pub executed_at_us: u64,
    pub fault_code: u16,
}

impl ActuatorStatus {
    /// Encodes an actuator status into the fixed little-endian layout.
    pub fn encode(self) -> Result<[u8; ACTUATOR_STATUS_PAYLOAD_LEN], VisionError> {
        self.validate()?;
        let mut bytes = [0u8; ACTUATOR_STATUS_PAYLOAD_LEN];
        bytes[0] = VISION_PAYLOAD_VERSION;
        bytes[1] = self.state as u8;
        bytes[2] = self.requested_action as u8;
        bytes[3] = self.actual_action as u8;
        bytes[4..8].copy_from_slice(&self.frame_id.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.applied_sequence.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.decision_age_at_send_us.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.local_apply_latency_us.to_le_bytes());
        bytes[20..28].copy_from_slice(&self.executed_at_us.to_le_bytes());
        bytes[28..30].copy_from_slice(&self.fault_code.to_le_bytes());
        Ok(bytes)
    }

    /// Decodes and validates one complete actuator-status payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, VisionError> {
        expect_len(bytes, ACTUATOR_STATUS_PAYLOAD_LEN)?;
        if bytes[0] != VISION_PAYLOAD_VERSION {
            return Err(VisionError::UnsupportedPayloadVersion(bytes[0]));
        }
        if bytes[30..32] != [0, 0] {
            return Err(VisionError::NonzeroReservedField);
        }
        let status = Self {
            state: ActuatorState::try_from(bytes[1])?,
            requested_action: VisionAction::try_from(bytes[2])?,
            actual_action: VisionAction::try_from(bytes[3])?,
            frame_id: read_u32(bytes, 4),
            applied_sequence: read_u32(bytes, 8),
            decision_age_at_send_us: read_u32(bytes, 12),
            local_apply_latency_us: read_u32(bytes, 16),
            executed_at_us: read_u64(bytes, 20),
            fault_code: read_u16(bytes, 28),
        };
        status.validate()?;
        Ok(status)
    }

    fn validate(self) -> Result<(), VisionError> {
        if self.frame_id == 0 || self.applied_sequence == 0 {
            return Err(VisionError::InvalidStatusIdentifier);
        }
        if ErrorCode::try_from(self.fault_code).is_err() {
            return Err(VisionError::UnsupportedFaultCode(self.fault_code));
        }
        match self.state {
            ActuatorState::Applied if self.fault_code != 0 => {
                Err(VisionError::InconsistentActuatorStatus)
            }
            ActuatorState::SafeFallback if !self.actual_action.is_safe() => {
                Err(VisionError::InconsistentActuatorStatus)
            }
            ActuatorState::Fault if self.fault_code == 0 => {
                Err(VisionError::InconsistentActuatorStatus)
            }
            _ => Ok(()),
        }
    }
}

/// RTOS-side visual actuator state after transport de-duplication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VisionEndpoint {
    timeout_us: u64,
    last_sequence: u32,
    last_frame_id: u32,
    last_valid_decision_us: Option<u64>,
    last_safe_action: VisionAction,
    last_requested_action: VisionAction,
    last_actual_action: VisionAction,
    last_decision_age_at_send_us: u32,
    timeout_reported: bool,
}

impl VisionEndpoint {
    /// Creates an endpoint whose silence timeout is measured on the RTOS clock.
    pub const fn new(timeout_us: u64) -> Self {
        Self {
            timeout_us,
            last_sequence: 0,
            last_frame_id: 0,
            last_valid_decision_us: None,
            last_safe_action: VisionAction::Hold,
            last_requested_action: VisionAction::Hold,
            last_actual_action: VisionAction::Hold,
            last_decision_age_at_send_us: 0,
            timeout_reported: false,
        }
    }

    /// Applies one fresh decision. Sender and receiver timestamps are never subtracted.
    pub fn apply(
        &mut self,
        sequence: u32,
        decision: VisionDecision,
        sender_sent_at_us: u64,
        received_at_us: u64,
    ) -> Result<ActuatorStatus, VisionError> {
        decision.validate()?;
        if sequence == 0 || sequence <= self.last_sequence {
            return Err(VisionError::ReplayedSequence(sequence));
        }
        if decision.frame_id <= self.last_frame_id {
            return Err(VisionError::ReplayedFrame(decision.frame_id));
        }
        let age_at_send = sender_sent_at_us
            .checked_sub(decision.inference_finished_at_us)
            .ok_or(VisionError::FutureInferenceTimestamp)?;
        if age_at_send > u64::from(decision.ttl_us) {
            return Err(VisionError::ExpiredDecision);
        }
        let age_at_send = u32::try_from(age_at_send).map_err(|_| VisionError::ExpiredDecision)?;

        self.last_sequence = sequence;
        self.last_frame_id = decision.frame_id;
        self.last_valid_decision_us = Some(received_at_us);
        self.last_safe_action = decision.safe_action;
        self.last_requested_action = decision.requested_action;
        self.last_actual_action = decision.requested_action;
        self.last_decision_age_at_send_us = age_at_send;
        self.timeout_reported = false;
        Ok(ActuatorStatus {
            state: ActuatorState::Applied,
            requested_action: decision.requested_action,
            actual_action: decision.requested_action,
            frame_id: decision.frame_id,
            applied_sequence: sequence,
            decision_age_at_send_us: age_at_send,
            local_apply_latency_us: 0,
            executed_at_us: received_at_us,
            fault_code: ErrorCode::None as u16,
        })
    }

    /// Returns a single safe-fallback transition after controller silence.
    pub fn check_timeout(&mut self, now_us: u64) -> Option<ActuatorStatus> {
        let last_valid_us = self.last_valid_decision_us?;
        let silence_us = now_us.checked_sub(last_valid_us)?;
        if silence_us <= self.timeout_us || self.timeout_reported {
            return None;
        }
        self.timeout_reported = true;
        self.last_actual_action = self.last_safe_action;
        Some(ActuatorStatus {
            state: ActuatorState::SafeFallback,
            requested_action: self.last_requested_action,
            actual_action: self.last_safe_action,
            frame_id: self.last_frame_id,
            applied_sequence: self.last_sequence,
            decision_age_at_send_us: self.last_decision_age_at_send_us,
            local_apply_latency_us: 0,
            executed_at_us: now_us,
            fault_code: ErrorCode::ControllerTimeout as u16,
        })
    }
}

/// Validation failure for visual payloads and actuator transitions.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum VisionError {
    #[error("payload length must be {expected} bytes, received {actual}")]
    InvalidLength { expected: usize, actual: usize },
    #[error("unsupported visual payload version {0}")]
    UnsupportedPayloadVersion(u8),
    #[error("unsupported visual action {0}")]
    UnsupportedAction(u8),
    #[error("unsupported actuator state {0}")]
    UnsupportedActuatorState(u8),
    #[error("unsupported visual flags 0x{0:02x}")]
    UnsupportedFlags(u8),
    #[error("reserved payload field is nonzero")]
    NonzeroReservedField,
    #[error("safe fallback action must be hold or emergency stop")]
    UnsafeFallbackAction,
    #[error("frame identifier must be nonzero")]
    ZeroFrameId,
    #[error("capture timestamp follows inference completion")]
    InvalidTimestampOrder,
    #[error("decision TTL {0} is outside 1..=5000000 microseconds")]
    InvalidTtl(u32),
    #[error("confidence {0} is outside 0..=10000")]
    InvalidConfidence(u16),
    #[error("detection flag and detection fields are inconsistent")]
    InconsistentDetection,
    #[error("status frame and sequence identifiers must be nonzero")]
    InvalidStatusIdentifier,
    #[error("unsupported actuator fault code {0}")]
    UnsupportedFaultCode(u16),
    #[error("actuator state, action, and fault are inconsistent")]
    InconsistentActuatorStatus,
    #[error("sequence {0} was already applied")]
    ReplayedSequence(u32),
    #[error("frame {0} was already applied")]
    ReplayedFrame(u32),
    #[error("inference completion timestamp follows the sender timestamp")]
    FutureInferenceTimestamp,
    #[error("vision decision expired before it was sent")]
    ExpiredDecision,
}

fn expect_len(bytes: &[u8], expected: usize) -> Result<(), VisionError> {
    if bytes.len() != expected {
        return Err(VisionError::InvalidLength {
            expected,
            actual: bytes.len(),
        });
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}
