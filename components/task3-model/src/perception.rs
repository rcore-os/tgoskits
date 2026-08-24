//! Bounded perception-to-control decisions shared by Task-3 model adapters.
//!
//! The detector runtime is deliberately outside this module.  A YOLO/ONNX,
//! TorchScript, or hardware-NPU adapter supplies normalized detection fields;
//! this module validates them and converts one accepted detection into a
//! control target without allowing model output to bypass the RTOS safety
//! contract.

/// A normalized YOLO-style detection produced by a model adapter.
///
/// All values use integer thousandths so the contract is deterministic in the
/// `no_std` guest and does not depend on floating-point rounding.  Coordinates
/// and area are normalized to `0..=1000`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YoloDetection {
    pub class_id: u16,
    pub confidence_milli: u16,
    pub center_x_milli: u16,
    pub center_y_milli: u16,
    pub area_milli: u16,
}

/// Fixed YOLO11n fixture metadata used by the no-std replay adapter.
///
/// The actual ONNX inference runs in `scripts/task3/run_yolo_fixture.py`.
/// This small deterministic adapter replays the three archived detections so
/// the AArch64 controller can exercise the same bounded contract without
/// embedding an ONNX runtime in the Guest image.
pub const YOLO_FIXTURE_MODEL: &str = "yolo11n.onnx";
pub const YOLO_FIXTURE_VERSION: &str = "ultralytics-yolo11n-v8.3.0";
pub const YOLO_FIXTURE_SHA256: &str =
    "634279b40c07c6391472c51ad45b81ebc48706a9a1fe72dd3396322acd0c053b";

/// Return one of the archived fixture observations in capture order.
///
/// The sequence mirrors `results/task3/yolo/yolo-fixture-manifest.json`:
/// close/no-detection, plant/normal detection, and black-box/large target
/// step.  `None` is intentional and exercises the safe hold-last-target path.
pub const fn yolo_fixture_detection(sample: u32) -> Option<YoloDetection> {
    match sample % 3 {
        0 => None,
        1 => Some(YoloDetection {
            class_id: 75,
            confidence_milli: 832,
            center_x_milli: 419,
            center_y_milli: 503,
            area_milli: 61,
        }),
        _ => Some(YoloDetection {
            class_id: 58,
            confidence_milli: 871,
            center_x_milli: 805,
            center_y_milli: 506,
            area_milli: 29,
        }),
    }
}

/// Policy limiting how a detection may affect the control target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YoloPolicy {
    pub min_confidence_milli: u16,
    pub min_area_milli: u16,
    pub target_min: i32,
    pub target_max: i32,
    pub max_target_step: i32,
}

impl YoloPolicy {
    /// Conservative default for the Task-3 `0..=1000` target range.
    pub const fn task3_default() -> Self {
        Self {
            min_confidence_milli: 600,
            min_area_milli: 10,
            target_min: 0,
            target_max: 1000,
            max_target_step: 100,
        }
    }
}

/// Why a model result was not allowed to drive the control loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerceptionRejectReason {
    InvalidConfidence,
    InvalidCoordinate,
    InvalidArea,
    LowConfidence,
    SmallArea,
    InvalidTargetRange,
}

/// A validated decision for the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerceptionDecision {
    /// Apply the bounded target and retain the detector confidence for logs.
    Target {
        target: i32,
        class_id: u16,
        confidence_milli: u16,
    },
    /// Enter the caller's Safe behavior; the reason must be observable.
    Reject(PerceptionRejectReason),
}

/// Errors found while decoding a YOLOv8 channel-first output tensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YoloTensorError {
    InvalidShape,
    NonFiniteValue,
}

/// Return the highest-confidence detection from a YOLOv8-style output.
///
/// `data` is one batch in channel-first layout:
/// `[cx, cy, w, h, class_0, ..., class_n] x rows`.  The model runtime is
/// responsible for resize/letterbox correction and NMS; this small decoder
/// only makes the tensor-to-contract conversion deterministic and bounded.
pub fn top_yolo_detection(
    data: &[f32],
    rows: usize,
    class_count: usize,
    input_width: u32,
    input_height: u32,
    confidence_threshold_milli: u16,
) -> Result<Option<YoloDetection>, YoloTensorError> {
    if rows == 0 || class_count == 0 || input_width == 0 || input_height == 0 {
        return Err(YoloTensorError::InvalidShape);
    }
    let channels = 4usize
        .checked_add(class_count)
        .ok_or(YoloTensorError::InvalidShape)?;
    let expected_len = channels
        .checked_mul(rows)
        .ok_or(YoloTensorError::InvalidShape)?;
    if data.len() != expected_len {
        return Err(YoloTensorError::InvalidShape);
    }

    let mut best: Option<(usize, f32)> = None;
    for row in 0..rows {
        for class_id in 0..class_count {
            let score = data[(4 + class_id) * rows + row];
            if !score.is_finite() {
                return Err(YoloTensorError::NonFiniteValue);
            }
            if score >= f32::from(confidence_threshold_milli) / 1000.0
                && best.is_none_or(|(_, best_score)| score > best_score)
            {
                best = Some((row, score));
            }
        }
    }

    let Some((row, score)) = best else {
        return Ok(None);
    };
    let mut class_id = 0;
    for candidate in 1..class_count {
        let candidate_score = data[(4 + candidate) * rows + row];
        if candidate_score > data[(4 + class_id) * rows + row] {
            class_id = candidate;
        }
    }
    let cx = data[row];
    let cy = data[rows + row];
    let width = data[2 * rows + row];
    let height = data[3 * rows + row];
    if !cx.is_finite() || !cy.is_finite() || !width.is_finite() || !height.is_finite() {
        return Err(YoloTensorError::NonFiniteValue);
    }
    if width < 0.0 || height < 0.0 {
        return Ok(None);
    }

    let normalized_x = (cx / input_width as f32).clamp(0.0, 1.0);
    let normalized_area =
        (width * height / (input_width as f32 * input_height as f32)).clamp(0.0, 1.0);
    Ok(Some(YoloDetection {
        class_id: class_id.min(u16::MAX as usize) as u16,
        confidence_milli: scaled_milli(score.clamp(0.0, 1.0)),
        center_x_milli: scaled_milli(normalized_x),
        center_y_milli: scaled_milli((cy / input_height as f32).clamp(0.0, 1.0)),
        area_milli: scaled_milli(normalized_area),
    }))
}

#[inline]
fn scaled_milli(value: f32) -> u16 {
    // `no_std` targets do not expose the floating-point `round` method in the
    // core-only build; adding 0.5 before truncation gives deterministic
    // nearest-integer conversion for the already-clamped non-negative range.
    (value * 1000.0 + 0.5) as u16
}

/// Convert one normalized YOLO detection into a bounded Task-3 target.
///
/// The x-center maps linearly into the configured target interval.  The
/// maximum step is applied around `current_target` so a single noisy frame
/// cannot create an unbounded control jump.  No detection aggregation or
/// temporal smoothing is hidden here; those policies belong to the caller.
pub fn yolo_detection_to_target(
    detection: YoloDetection,
    current_target: i32,
    policy: YoloPolicy,
) -> PerceptionDecision {
    if detection.confidence_milli > 1000 {
        return PerceptionDecision::Reject(PerceptionRejectReason::InvalidConfidence);
    }
    if detection.center_x_milli > 1000 || detection.center_y_milli > 1000 {
        return PerceptionDecision::Reject(PerceptionRejectReason::InvalidCoordinate);
    }
    if detection.area_milli > 1000 {
        return PerceptionDecision::Reject(PerceptionRejectReason::InvalidArea);
    }
    if policy.target_min > policy.target_max || policy.max_target_step < 0 {
        return PerceptionDecision::Reject(PerceptionRejectReason::InvalidTargetRange);
    }
    if detection.confidence_milli < policy.min_confidence_milli {
        return PerceptionDecision::Reject(PerceptionRejectReason::LowConfidence);
    }
    if detection.area_milli < policy.min_area_milli {
        return PerceptionDecision::Reject(PerceptionRejectReason::SmallArea);
    }

    let target_min = i64::from(policy.target_min);
    let target_max = i64::from(policy.target_max);
    let target_span = target_max - target_min;
    let mapped_target =
        i64::from(policy.target_min) + target_span * i64::from(detection.center_x_milli) / 1000;
    let bounded_current = i64::from(current_target).clamp(target_min, target_max);
    let step = i64::from(policy.max_target_step);
    let target = mapped_target
        .clamp(bounded_current - step, bounded_current + step)
        .clamp(target_min, target_max) as i32;

    PerceptionDecision::Target {
        target,
        class_id: detection.class_id,
        confidence_milli: detection.confidence_milli,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(center_x_milli: u16) -> YoloDetection {
        YoloDetection {
            class_id: 1,
            confidence_milli: 900,
            center_x_milli,
            center_y_milli: 500,
            area_milli: 200,
        }
    }

    #[test]
    fn fixture_replay_matches_archived_detection_sequence() {
        assert_eq!(yolo_fixture_detection(0), None);
        assert_eq!(
            yolo_fixture_detection(1),
            Some(YoloDetection {
                class_id: 75,
                confidence_milli: 832,
                center_x_milli: 419,
                center_y_milli: 503,
                area_milli: 61,
            })
        );
        assert_eq!(
            yolo_fixture_detection(2),
            Some(YoloDetection {
                class_id: 58,
                confidence_milli: 871,
                center_x_milli: 805,
                center_y_milli: 506,
                area_milli: 29,
            })
        );
    }

    #[test]
    fn fixture_replay_targets_match_manifest_contract_anchor() {
        let policy = YoloPolicy::task3_default();
        assert_eq!(
            yolo_detection_to_target(yolo_fixture_detection(1).unwrap(), 500, policy),
            PerceptionDecision::Target {
                target: 419,
                class_id: 75,
                confidence_milli: 832,
            }
        );
        assert_eq!(
            yolo_detection_to_target(yolo_fixture_detection(2).unwrap(), 500, policy),
            PerceptionDecision::Target {
                target: 600,
                class_id: 58,
                confidence_milli: 871,
            }
        );
    }

    #[test]
    fn maps_center_and_preserves_detection_metadata() {
        assert_eq!(
            yolo_detection_to_target(detection(500), 500, YoloPolicy::task3_default()),
            PerceptionDecision::Target {
                target: 500,
                class_id: 1,
                confidence_milli: 900,
            }
        );
    }

    #[test]
    fn rejects_low_confidence_and_small_area() {
        let mut low_confidence = detection(500);
        low_confidence.confidence_milli = 599;
        assert_eq!(
            yolo_detection_to_target(low_confidence, 500, YoloPolicy::task3_default()),
            PerceptionDecision::Reject(PerceptionRejectReason::LowConfidence)
        );

        let mut small_area = detection(500);
        small_area.area_milli = 9;
        assert_eq!(
            yolo_detection_to_target(small_area, 500, YoloPolicy::task3_default()),
            PerceptionDecision::Reject(PerceptionRejectReason::SmallArea)
        );
    }

    #[test]
    fn rejects_out_of_range_normalized_fields() {
        let mut invalid = detection(500);
        invalid.center_x_milli = 1001;
        assert_eq!(
            yolo_detection_to_target(invalid, 500, YoloPolicy::task3_default()),
            PerceptionDecision::Reject(PerceptionRejectReason::InvalidCoordinate)
        );

        let mut invalid = detection(500);
        invalid.center_y_milli = 1001;
        assert_eq!(
            yolo_detection_to_target(invalid, 500, YoloPolicy::task3_default()),
            PerceptionDecision::Reject(PerceptionRejectReason::InvalidCoordinate)
        );

        let mut invalid = detection(500);
        invalid.confidence_milli = 1001;
        assert_eq!(
            yolo_detection_to_target(invalid, 500, YoloPolicy::task3_default()),
            PerceptionDecision::Reject(PerceptionRejectReason::InvalidConfidence)
        );
    }

    #[test]
    fn clamps_single_frame_target_step() {
        assert_eq!(
            yolo_detection_to_target(detection(1000), 500, YoloPolicy::task3_default()),
            PerceptionDecision::Target {
                target: 600,
                class_id: 1,
                confidence_milli: 900,
            }
        );
    }

    #[test]
    fn rejects_invalid_policy_range() {
        let mut policy = YoloPolicy::task3_default();
        policy.target_min = 1000;
        policy.target_max = 0;
        assert_eq!(
            yolo_detection_to_target(detection(500), 500, policy),
            PerceptionDecision::Reject(PerceptionRejectReason::InvalidTargetRange)
        );
    }

    #[test]
    fn decodes_highest_yolov8_candidate() {
        // Two rows, one class: [cx, cy, w, h, score] channel-first.
        let output = [
            80.0, 240.0, // cx
            100.0, 120.0, // cy
            40.0, 80.0, // width
            40.0, 80.0, // height
            0.4, 0.9, // class score
        ];
        assert_eq!(
            top_yolo_detection(&output, 2, 1, 320, 320, 500),
            Ok(Some(YoloDetection {
                class_id: 0,
                confidence_milli: 900,
                center_x_milli: 750,
                center_y_milli: 375,
                area_milli: 63,
            }))
        );
    }

    #[test]
    fn rejects_malformed_or_non_finite_yolov8_output() {
        assert_eq!(
            top_yolo_detection(&[0.0; 5], 0, 1, 320, 320, 500),
            Err(YoloTensorError::InvalidShape)
        );
        let mut output = [0.0f32; 5];
        output[4] = f32::NAN;
        assert_eq!(
            top_yolo_detection(&output, 1, 1, 320, 320, 500),
            Err(YoloTensorError::NonFiniteValue)
        );
    }
}
