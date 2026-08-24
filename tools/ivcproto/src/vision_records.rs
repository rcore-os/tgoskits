//! Parser for machine-readable decisions emitted by the RKNN image runner.

use thiserror::Error;

use crate::vision::{BoundingBox, VisionAction, VisionDecision, VisionError};

const RECORD_PREFIX: &str = "VISION_DECISION_RECORD";
const FIELD_COUNT: usize = 15;

/// Parses one runner line, returning `None` for unrelated human-readable output.
pub fn parse_decision_record(line: &str) -> Result<Option<VisionDecision>, VisionRecordError> {
    let mut tokens = line.split_whitespace();
    if tokens.next() != Some(RECORD_PREFIX) {
        return Ok(None);
    }
    let fields: [&str; FIELD_COUNT] = core::array::from_fn(|_| tokens.next().unwrap_or(""));
    let actual = fields.iter().take_while(|field| !field.is_empty()).count() + tokens.count();
    if actual != FIELD_COUNT {
        return Err(VisionRecordError::InvalidFieldCount { actual });
    }

    let version = parse_u8(fields[0], "version")?;
    if version != 1 {
        return Err(VisionRecordError::UnsupportedVersion(version));
    }
    let detection_present = match parse_u8(fields[7], "detection_present")? {
        0 => false,
        1 => true,
        other => return Err(VisionRecordError::InvalidDetectionFlag(other)),
    };
    let decision = VisionDecision {
        requested_action: parse_action(field_value(fields[5], "requested_action")?)?,
        safe_action: parse_action(field_value(fields[6], "safe_action")?)?,
        detection_present,
        frame_id: parse_u32(fields[1], "frame_id")?,
        captured_at_us: parse_u64(fields[2], "captured_at_us")?,
        inference_finished_at_us: parse_u64(fields[3], "inference_finished_at_us")?,
        ttl_us: parse_u32(fields[4], "ttl_us")?,
        class_id: parse_u16(fields[8], "class_id")?,
        confidence_q10000: parse_u16(fields[9], "confidence_q10000")?,
        region_id: parse_u16(fields[10], "region_id")?,
        bounding_box: BoundingBox {
            left: parse_u16(fields[11], "left")?,
            top: parse_u16(fields[12], "top")?,
            right: parse_u16(fields[13], "right")?,
            bottom: parse_u16(fields[14], "bottom")?,
        },
    };
    decision
        .validate()
        .map_err(VisionRecordError::InvalidDecision)?;
    Ok(Some(decision))
}

fn field_value<'a>(
    field: &'a str,
    expected_name: &'static str,
) -> Result<&'a str, VisionRecordError> {
    let (name, value) = field
        .split_once('=')
        .ok_or(VisionRecordError::MalformedField(expected_name))?;
    if name != expected_name || value.is_empty() {
        return Err(VisionRecordError::MalformedField(expected_name));
    }
    Ok(value)
}

fn parse_u8(field: &str, name: &'static str) -> Result<u8, VisionRecordError> {
    field_value(field, name)?
        .parse()
        .map_err(|_| VisionRecordError::InvalidInteger(name))
}

fn parse_u16(field: &str, name: &'static str) -> Result<u16, VisionRecordError> {
    field_value(field, name)?
        .parse()
        .map_err(|_| VisionRecordError::InvalidInteger(name))
}

fn parse_u32(field: &str, name: &'static str) -> Result<u32, VisionRecordError> {
    field_value(field, name)?
        .parse()
        .map_err(|_| VisionRecordError::InvalidInteger(name))
}

fn parse_u64(field: &str, name: &'static str) -> Result<u64, VisionRecordError> {
    field_value(field, name)?
        .parse()
        .map_err(|_| VisionRecordError::InvalidInteger(name))
}

fn parse_action(value: &str) -> Result<VisionAction, VisionRecordError> {
    match value {
        "hold" => Ok(VisionAction::Hold),
        "left" => Ok(VisionAction::SortLeft),
        "right" => Ok(VisionAction::SortRight),
        "emergency-stop" => Ok(VisionAction::EmergencyStop),
        _ => Err(VisionRecordError::UnsupportedAction),
    }
}

/// Strict runner-record parsing failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum VisionRecordError {
    #[error("vision record must contain {FIELD_COUNT} fields, received {actual}")]
    InvalidFieldCount { actual: usize },
    #[error("vision record field '{0}' is missing, reordered, or malformed")]
    MalformedField(&'static str),
    #[error("vision record field '{0}' is not a valid integer")]
    InvalidInteger(&'static str),
    #[error("unsupported vision record version {0}")]
    UnsupportedVersion(u8),
    #[error("detection flag must be zero or one, received {0}")]
    InvalidDetectionFlag(u8),
    #[error("unsupported vision action")]
    UnsupportedAction,
    #[error("invalid visual decision: {0}")]
    InvalidDecision(VisionError),
}
