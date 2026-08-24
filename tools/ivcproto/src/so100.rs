//! Fixed-capability planning for the competition SO-100 ID1 profile.

use thiserror::Error;

use crate::vision::VisionAction;

pub const ID1_BASELINE_POSITION: u16 = 2042;
pub const ID1_RIGHT_POSITION: u16 = 2074;
pub const ID1_POSITION_TOLERANCE: u16 = 8;
pub const REQUIRED_STABLE_AUTHORIZATIONS: u8 = 3;

const AUTH_RECORD_PREFIX: &str = "VISION_RTOS_AUTH_RECORD";
const AUTH_FIELD_COUNT: usize = 8;
const SERVO_ID1: u8 = 1;
const FEETECH_WRITE: u8 = 0x03;
const ADDRESS_ACCELERATION: u8 = 41;
const ADDRESS_GOAL_POSITION: u8 = 42;
const ADDRESS_RUNNING_TIME: u8 = 44;
const ADDRESS_GOAL_VELOCITY: u8 = 46;
const ADDRESS_TORQUE_LIMIT: u8 = 48;
const ADDRESS_TORQUE_ENABLE: u8 = 40;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtosAuthorization {
    pub session_id: u32,
    pub sequence: u32,
    pub frame_id: u32,
    pub action: VisionAction,
    pub retries: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixedId1Command {
    Hold,
    MoveTo(u16),
    EmergencyStop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateOutcome {
    Pending {
        action: VisionAction,
        observed: u8,
        required: u8,
    },
    Stable(FixedId1Command),
    NoChange(VisionAction),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AuthorizationGate {
    session_id: Option<u32>,
    last_sequence: u32,
    last_frame_id: u32,
    candidate: Option<VisionAction>,
    candidate_count: u8,
    stable_action: Option<VisionAction>,
    emergency_stopped: bool,
}

impl AuthorizationGate {
    pub const fn new() -> Self {
        Self {
            session_id: None,
            last_sequence: 0,
            last_frame_id: 0,
            candidate: None,
            candidate_count: 0,
            stable_action: None,
            emergency_stopped: false,
        }
    }

    pub fn observe(&mut self, authorization: RtosAuthorization) -> Result<GateOutcome, So100Error> {
        if self.emergency_stopped {
            return Err(So100Error::EmergencyStopLatched);
        }
        if authorization.session_id == 0
            || authorization.sequence == 0
            || authorization.frame_id == 0
        {
            return Err(So100Error::InvalidIdentity);
        }
        match self.session_id {
            Some(session_id) if session_id != authorization.session_id => {
                return Err(So100Error::SessionChanged);
            }
            None => self.session_id = Some(authorization.session_id),
            _ => {}
        }
        let expected_sequence = self
            .last_sequence
            .checked_add(1)
            .ok_or(So100Error::SequenceExhausted)?;
        if authorization.sequence != expected_sequence {
            return Err(So100Error::NonConsecutiveSequence {
                expected: expected_sequence,
                actual: authorization.sequence,
            });
        }
        if authorization.frame_id <= self.last_frame_id {
            return Err(So100Error::NonIncreasingFrame);
        }
        self.last_sequence = authorization.sequence;
        self.last_frame_id = authorization.frame_id;

        if authorization.action == VisionAction::EmergencyStop {
            self.emergency_stopped = true;
            self.stable_action = Some(VisionAction::EmergencyStop);
            return Ok(GateOutcome::Stable(FixedId1Command::EmergencyStop));
        }

        if self.candidate == Some(authorization.action) {
            self.candidate_count = self.candidate_count.saturating_add(1);
        } else {
            self.candidate = Some(authorization.action);
            self.candidate_count = 1;
        }
        if self.candidate_count < REQUIRED_STABLE_AUTHORIZATIONS {
            return Ok(GateOutcome::Pending {
                action: authorization.action,
                observed: self.candidate_count,
                required: REQUIRED_STABLE_AUTHORIZATIONS,
            });
        }
        if self.stable_action == Some(authorization.action) {
            return Ok(GateOutcome::NoChange(authorization.action));
        }
        self.stable_action = Some(authorization.action);
        Ok(GateOutcome::Stable(command_for_action(
            authorization.action,
        )))
    }
}

pub const fn command_for_action(action: VisionAction) -> FixedId1Command {
    match action {
        VisionAction::Hold => FixedId1Command::Hold,
        VisionAction::SortLeft => FixedId1Command::MoveTo(ID1_BASELINE_POSITION),
        VisionAction::SortRight => FixedId1Command::MoveTo(ID1_RIGHT_POSITION),
        VisionAction::EmergencyStop => FixedId1Command::EmergencyStop,
    }
}

pub fn parse_rtos_authorization(line: &str) -> Result<Option<RtosAuthorization>, So100RecordError> {
    let mut tokens = line.split_whitespace();
    if tokens.next() != Some(AUTH_RECORD_PREFIX) {
        return Ok(None);
    }
    let fields: [&str; AUTH_FIELD_COUNT] = core::array::from_fn(|_| tokens.next().unwrap_or(""));
    let actual = fields.iter().take_while(|field| !field.is_empty()).count() + tokens.count();
    if actual != AUTH_FIELD_COUNT {
        return Err(So100RecordError::InvalidFieldCount { actual });
    }
    let version = parse_u8(fields[0], "version")?;
    if version != 1 {
        return Err(So100RecordError::UnsupportedVersion(version));
    }
    let requested = parse_action(field_value(fields[4], "requested_action")?)?;
    let authorized = parse_action(field_value(fields[5], "authorized_action")?)?;
    if requested != authorized {
        return Err(So100RecordError::AuthorizationMismatch);
    }
    if field_value(fields[6], "state")? != "applied" {
        return Err(So100RecordError::AuthorizationNotApplied);
    }
    Ok(Some(RtosAuthorization {
        session_id: parse_u32(fields[1], "session_id")?,
        sequence: parse_u32(fields[2], "sequence")?,
        frame_id: parse_u32(fields[3], "frame_id")?,
        action: authorized,
        retries: parse_u32(fields[7], "retries")?,
    }))
}

/// A packet in the Feetech protocol-0 wire layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Id1Packet {
    bytes: [u8; 16],
    length: u8,
}

impl Id1Packet {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Id1Write {
    ConfigureAcceleration,
    ConfigureRunningTime,
    ConfigureVelocity,
    ConfigureTorqueLimit,
    EnableTorque,
    GoalBaseline,
    GoalRight,
    DisableTorque,
}

/// Encodes only the writes permitted by the frozen ID1 competition profile.
pub fn encode_id1_write(write: Id1Write) -> Id1Packet {
    match write {
        Id1Write::ConfigureAcceleration => write_u8(ADDRESS_ACCELERATION, 5),
        Id1Write::ConfigureRunningTime => write_u16(ADDRESS_RUNNING_TIME, 0),
        Id1Write::ConfigureVelocity => write_u16(ADDRESS_GOAL_VELOCITY, 10),
        Id1Write::ConfigureTorqueLimit => write_u16(ADDRESS_TORQUE_LIMIT, 200),
        Id1Write::EnableTorque => write_u8(ADDRESS_TORQUE_ENABLE, 1),
        Id1Write::GoalBaseline => write_u16(ADDRESS_GOAL_POSITION, ID1_BASELINE_POSITION),
        Id1Write::GoalRight => write_u16(ADDRESS_GOAL_POSITION, ID1_RIGHT_POSITION),
        Id1Write::DisableTorque => write_u8(ADDRESS_TORQUE_ENABLE, 0),
    }
}

fn write_u8(address: u8, value: u8) -> Id1Packet {
    encode_write(&[address, value])
}

fn write_u16(address: u8, value: u16) -> Id1Packet {
    let value = value.to_le_bytes();
    encode_write(&[address, value[0], value[1]])
}

fn encode_write(parameters: &[u8]) -> Id1Packet {
    let mut bytes = [0u8; 16];
    bytes[0] = 0xff;
    bytes[1] = 0xff;
    bytes[2] = SERVO_ID1;
    bytes[3] = u8::try_from(parameters.len() + 2).expect("fixed ID1 packet length fits u8");
    bytes[4] = FEETECH_WRITE;
    bytes[5..5 + parameters.len()].copy_from_slice(parameters);
    let checksum_index = 5 + parameters.len();
    bytes[checksum_index] = checksum(&bytes[2..checksum_index]);
    Id1Packet {
        bytes,
        length: u8::try_from(checksum_index + 1).expect("fixed ID1 packet length fits u8"),
    }
}

fn checksum(bytes: &[u8]) -> u8 {
    !bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

fn field_value<'a>(
    field: &'a str,
    expected_name: &'static str,
) -> Result<&'a str, So100RecordError> {
    let (name, value) = field
        .split_once('=')
        .ok_or(So100RecordError::MalformedField(expected_name))?;
    if name != expected_name || value.is_empty() {
        return Err(So100RecordError::MalformedField(expected_name));
    }
    Ok(value)
}

fn parse_u8(field: &str, name: &'static str) -> Result<u8, So100RecordError> {
    field_value(field, name)?
        .parse()
        .map_err(|_| So100RecordError::InvalidInteger(name))
}

fn parse_u32(field: &str, name: &'static str) -> Result<u32, So100RecordError> {
    field_value(field, name)?
        .parse()
        .map_err(|_| So100RecordError::InvalidInteger(name))
}

fn parse_action(value: &str) -> Result<VisionAction, So100RecordError> {
    match value {
        "hold" => Ok(VisionAction::Hold),
        "left" => Ok(VisionAction::SortLeft),
        "right" => Ok(VisionAction::SortRight),
        "emergency-stop" => Ok(VisionAction::EmergencyStop),
        _ => Err(So100RecordError::UnsupportedAction),
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum So100Error {
    #[error("authorization identity must be nonzero")]
    InvalidIdentity,
    #[error("authorization session changed without rearming")]
    SessionChanged,
    #[error("authorization sequence is not consecutive: expected {expected}, received {actual}")]
    NonConsecutiveSequence { expected: u32, actual: u32 },
    #[error("authorization sequence space is exhausted")]
    SequenceExhausted,
    #[error("authorization frame did not increase")]
    NonIncreasingFrame,
    #[error("emergency stop is latched")]
    EmergencyStopLatched,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum So100RecordError {
    #[error("RTOS authorization record must contain {AUTH_FIELD_COUNT} fields, received {actual}")]
    InvalidFieldCount { actual: usize },
    #[error("RTOS authorization field '{0}' is missing, reordered, or malformed")]
    MalformedField(&'static str),
    #[error("RTOS authorization field '{0}' is not a valid integer")]
    InvalidInteger(&'static str),
    #[error("unsupported RTOS authorization record version {0}")]
    UnsupportedVersion(u8),
    #[error("unsupported RTOS authorization action")]
    UnsupportedAction,
    #[error("requested and authorized actions differ")]
    AuthorizationMismatch,
    #[error("RTOS authorization state is not applied")]
    AuthorizationNotApplied,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authorization(sequence: u32, frame_id: u32, action: VisionAction) -> RtosAuthorization {
        RtosAuthorization {
            session_id: 7,
            sequence,
            frame_id,
            action,
            retries: 0,
        }
    }

    #[test]
    fn parser_requires_matching_applied_authorization() {
        let line = concat!(
            "VISION_RTOS_AUTH_RECORD version=1 session_id=7 sequence=9 frame_id=42 ",
            "requested_action=right authorized_action=right state=applied retries=1"
        );
        assert_eq!(
            parse_rtos_authorization(line),
            Ok(Some(RtosAuthorization {
                session_id: 7,
                sequence: 9,
                frame_id: 42,
                action: VisionAction::SortRight,
                retries: 1,
            }))
        );

        let mismatch = line.replace("authorized_action=right", "authorized_action=hold");
        assert_eq!(
            parse_rtos_authorization(&mismatch),
            Err(So100RecordError::AuthorizationMismatch)
        );
    }

    #[test]
    fn gate_requires_three_frames_and_never_repeats_stable_goal() {
        let mut gate = AuthorizationGate::new();
        assert_eq!(
            gate.observe(authorization(1, 11, VisionAction::SortRight)),
            Ok(GateOutcome::Pending {
                action: VisionAction::SortRight,
                observed: 1,
                required: 3,
            })
        );
        assert!(matches!(
            gate.observe(authorization(2, 12, VisionAction::SortRight)),
            Ok(GateOutcome::Pending { observed: 2, .. })
        ));
        assert_eq!(
            gate.observe(authorization(3, 13, VisionAction::SortRight)),
            Ok(GateOutcome::Stable(FixedId1Command::MoveTo(2074)))
        );
        assert_eq!(
            gate.observe(authorization(4, 14, VisionAction::SortRight)),
            Ok(GateOutcome::NoChange(VisionAction::SortRight))
        );
    }

    #[test]
    fn gate_rejects_a_missing_authorization_sequence_before_stability() {
        let mut gate = AuthorizationGate::new();
        assert!(matches!(
            gate.observe(authorization(1, 11, VisionAction::SortRight)),
            Ok(GateOutcome::Pending { .. })
        ));

        assert!(
            gate.observe(authorization(3, 13, VisionAction::SortRight))
                .is_err()
        );
    }

    #[test]
    fn gate_maps_left_and_absent_hold_after_stability() {
        let mut gate = AuthorizationGate::new();
        for sequence in 1..=2 {
            assert!(matches!(
                gate.observe(authorization(
                    sequence,
                    sequence + 10,
                    VisionAction::SortLeft
                )),
                Ok(GateOutcome::Pending { .. })
            ));
        }
        assert_eq!(
            gate.observe(authorization(3, 13, VisionAction::SortLeft)),
            Ok(GateOutcome::Stable(FixedId1Command::MoveTo(2042)))
        );
        for sequence in 4..=5 {
            assert!(matches!(
                gate.observe(authorization(sequence, sequence + 10, VisionAction::Hold)),
                Ok(GateOutcome::Pending { .. })
            ));
        }
        assert_eq!(
            gate.observe(authorization(6, 16, VisionAction::Hold)),
            Ok(GateOutcome::Stable(FixedId1Command::Hold))
        );
    }

    #[test]
    fn emergency_stop_is_immediate_and_latched() {
        let mut gate = AuthorizationGate::new();
        assert_eq!(
            gate.observe(authorization(1, 1, VisionAction::EmergencyStop)),
            Ok(GateOutcome::Stable(FixedId1Command::EmergencyStop))
        );
        assert_eq!(
            gate.observe(authorization(2, 2, VisionAction::Hold)),
            Err(So100Error::EmergencyStopLatched)
        );
    }

    #[test]
    fn packet_encoder_exposes_only_frozen_id1_writes() {
        assert_eq!(
            encode_id1_write(Id1Write::GoalRight).as_bytes(),
            &[0xff, 0xff, 1, 5, 3, 42, 0x1a, 0x08, 0xaa]
        );
        assert_eq!(
            encode_id1_write(Id1Write::DisableTorque).as_bytes(),
            &[0xff, 0xff, 1, 4, 3, 40, 0, 0xcf]
        );
    }
}
