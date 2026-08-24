//! Strict parser for the experiment-local RKNN-to-T2N1 event record.

use std::{fs, string::String};

use task3_model::perception::YoloDetection;

pub(crate) const RECORD_VERSION: u16 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum EventKind {
    Detection(YoloDetection),
    NoDetection,
    Fixed { target: i32 },
    Reset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Event {
    pub(crate) generation: u64,
    pub(crate) event_index: u16,
    pub(crate) event_id: String,
    pub(crate) inference_start_ns: u64,
    pub(crate) inference_end_ns: u64,
    pub(crate) kind: EventKind,
}

pub(crate) fn read(path: &str) -> Result<Event, &'static str> {
    let contents = fs::read_to_string(path).map_err(|_| "event record is unavailable")?;
    parse(&contents)
}

pub(crate) fn parse(contents: &str) -> Result<Event, &'static str> {
    let mut version = None;
    let mut generation = None;
    let mut event_index = None;
    let mut event_id = None;
    let mut kind = None;
    let mut inference_start_ns = None;
    let mut inference_end_ns = None;
    let mut target = None;
    let mut class_id = None;
    let mut confidence_milli = None;
    let mut center_x_milli = None;
    let mut center_y_milli = None;
    let mut area_milli = None;

    for field in contents.split_ascii_whitespace() {
        let (key, value) = field.split_once('=').ok_or("field is missing '='")?;
        match key {
            "version" => assign(&mut version, parse_number(value)?)?,
            "generation" => assign(&mut generation, parse_number(value)?)?,
            "event_index" => assign(&mut event_index, parse_number(value)?)?,
            "event_id" => {
                if value.is_empty()
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                {
                    return Err("event_id contains unsupported characters");
                }
                assign(&mut event_id, value.to_owned())?;
            }
            "kind" => assign(&mut kind, value.to_owned())?,
            "infer_start_ns" => assign(&mut inference_start_ns, parse_number(value)?)?,
            "infer_end_ns" => assign(&mut inference_end_ns, parse_number(value)?)?,
            "target" => assign(&mut target, parse_number(value)?)?,
            "class" => assign(&mut class_id, parse_number(value)?)?,
            "confidence_milli" => assign(&mut confidence_milli, parse_number(value)?)?,
            "center_x_milli" => assign(&mut center_x_milli, parse_number(value)?)?,
            "center_y_milli" => assign(&mut center_y_milli, parse_number(value)?)?,
            "area_milli" => assign(&mut area_milli, parse_number(value)?)?,
            _ => return Err("event record contains an unknown field"),
        }
    }

    if version != Some(RECORD_VERSION) {
        return Err("unsupported event record version");
    }
    let inference_start_ns = inference_start_ns.ok_or("missing infer_start_ns")?;
    let inference_end_ns = inference_end_ns.ok_or("missing infer_end_ns")?;
    if inference_end_ns < inference_start_ns {
        return Err("inference timestamps are reversed");
    }
    let kind = match kind.as_deref().ok_or("missing kind")? {
        "detection" => EventKind::Detection(YoloDetection {
            class_id: required_range(class_id, 0, u64::from(u16::MAX), "invalid class")? as u16,
            confidence_milli: required_range(confidence_milli, 0, 1000, "invalid confidence_milli")?
                as u16,
            center_x_milli: required_range(center_x_milli, 0, 1000, "invalid center_x_milli")?
                as u16,
            center_y_milli: required_range(center_y_milli, 0, 1000, "invalid center_y_milli")?
                as u16,
            area_milli: required_range(area_milli, 0, 1000, "invalid area_milli")? as u16,
        }),
        "fixed" => EventKind::Fixed {
            target: required_range(target, 0, 1000, "invalid target")? as i32,
        },
        "no_detection" => {
            reject_detection_fields(
                target,
                class_id,
                confidence_milli,
                center_x_milli,
                center_y_milli,
                area_milli,
            )?;
            EventKind::NoDetection
        }
        "reset" => {
            reject_detection_fields(
                target,
                class_id,
                confidence_milli,
                center_x_milli,
                center_y_milli,
                area_milli,
            )?;
            EventKind::Reset
        }
        _ => return Err("unsupported event kind"),
    };
    if matches!(kind, EventKind::Detection(_)) && target.is_some() {
        return Err("detection record must not contain target");
    }
    if matches!(kind, EventKind::Fixed { .. })
        && [
            class_id,
            confidence_milli,
            center_x_milli,
            center_y_milli,
            area_milli,
        ]
        .into_iter()
        .any(|field| field.is_some())
    {
        return Err("fixed record contains detection fields");
    }

    Ok(Event {
        generation: generation.ok_or("missing generation")?,
        event_index: required_range(event_index, 1, u64::from(u16::MAX), "invalid event_index")?
            as u16,
        event_id: event_id.ok_or("missing event_id")?,
        inference_start_ns,
        inference_end_ns,
        kind,
    })
}

fn parse_number<T: core::str::FromStr>(value: &str) -> Result<T, &'static str> {
    value
        .parse()
        .map_err(|_| "field contains an invalid number")
}

fn assign<T>(slot: &mut Option<T>, value: T) -> Result<(), &'static str> {
    if slot.replace(value).is_some() {
        return Err("event record contains a duplicate field");
    }
    Ok(())
}

fn required_range(
    value: Option<u64>,
    minimum: u64,
    maximum: u64,
    error: &'static str,
) -> Result<u64, &'static str> {
    value
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or(error)
}

fn reject_detection_fields(
    target: Option<u64>,
    class_id: Option<u64>,
    confidence_milli: Option<u64>,
    center_x_milli: Option<u64>,
    center_y_milli: Option<u64>,
    area_milli: Option<u64>,
) -> Result<(), &'static str> {
    if [
        target,
        class_id,
        confidence_milli,
        center_x_milli,
        center_y_milli,
        area_milli,
    ]
    .into_iter()
    .any(|field| field.is_some())
    {
        return Err("non-data event contains data fields");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DETECTION: &str = "version=2 generation=7 event_index=3 event_id=road-0385 \
                             kind=detection infer_start_ns=100 infer_end_ns=140 class=2 \
                             confidence_milli=901 center_x_milli=480 center_y_milli=510 \
                             area_milli=80";

    #[test]
    fn parses_complete_detection_record() {
        let event = parse(DETECTION).unwrap();
        assert_eq!(event.generation, 7);
        assert_eq!(event.event_index, 3);
        assert_eq!(event.event_id, "road-0385");
        assert_eq!(event.inference_end_ns - event.inference_start_ns, 40);
        assert!(matches!(
            event.kind,
            EventKind::Detection(YoloDetection {
                class_id: 2,
                center_x_milli: 480,
                center_y_milli: 510,
                ..
            })
        ));
    }

    #[test]
    fn parses_fixed_and_reset_records() {
        assert!(matches!(
            parse(
                "version=2 generation=1 event_index=1 event_id=road-0375 kind=fixed \
                 infer_start_ns=10 infer_end_ns=10 target=500"
            )
            .unwrap()
            .kind,
            EventKind::Fixed { target: 500 }
        ));
        assert!(matches!(
            parse(
                "version=2 generation=11 event_index=11 event_id=explicit-reset kind=reset \
                 infer_start_ns=20 infer_end_ns=20"
            )
            .unwrap()
            .kind,
            EventKind::Reset
        ));
    }

    #[test]
    fn rejects_duplicate_unknown_missing_and_out_of_range_fields() {
        assert!(parse(&format!("{DETECTION} generation=8")).is_err());
        assert!(parse(&format!("{DETECTION} surprise=1")).is_err());
        assert!(parse(&DETECTION.replace(" event_id=road-0385", "")).is_err());
        assert!(parse(&DETECTION.replace("center_y_milli=510", "center_y_milli=1001")).is_err());
        assert!(parse(&DETECTION.replace("infer_end_ns=140", "infer_end_ns=99")).is_err());
    }
}
