//! Deterministic safety state for continuous Task-3 perception scenes.
//!
//! Model runtimes provide one normalized detection (or no detection) per
//! sampled video frame. This module owns the semantic and temporal policy:
//! vehicles may update the bounded target, hazards latch Stop, and repeated
//! unusable observations fail safe. Only an explicit Reset leaves Stop.

use crate::perception::{
    PerceptionDecision, PerceptionRejectReason, YoloDetection, YoloPolicy, yolo_detection_to_target,
};

pub const COCO_PERSON: u16 = 0;
pub const COCO_CAR: u16 = 2;
pub const COCO_BUS: u16 = 5;
pub const COCO_TRUCK: u16 = 7;
pub const COCO_KNIFE: u16 = 43;
pub const COCO_SCISSORS: u16 = 76;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoSafetyPolicy {
    pub perception: YoloPolicy,
    pub danger_x_min_milli: u16,
    pub danger_x_max_milli: u16,
    pub danger_y_min_milli: u16,
    pub danger_y_max_milli: u16,
    pub max_consecutive_misses: u16,
}

impl VideoSafetyPolicy {
    pub const fn task3_default() -> Self {
        Self {
            perception: YoloPolicy::task3_default(),
            danger_x_min_milli: 350,
            danger_x_max_milli: 650,
            danger_y_min_milli: 300,
            danger_y_max_milli: 1000,
            max_consecutive_misses: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneInput {
    Detection(YoloDetection),
    NoDetection,
    Reset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldReason {
    NoDetection,
    PerceptionRejected(PerceptionRejectReason),
    NonTrackingClass(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    HazardClass(u16),
    TrackingLost(HoldReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoSafetyState {
    Tracking,
    StoppedLatched(StopReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneDecision {
    Track {
        target: i32,
        class_id: u16,
        confidence_milli: u16,
    },
    Hold {
        target: i32,
        reason: HoldReason,
        consecutive_misses: u16,
    },
    Stop {
        reason: StopReason,
        newly_latched: bool,
    },
    Reset {
        target: i32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoSafetyController {
    policy: VideoSafetyPolicy,
    state: VideoSafetyState,
    current_target: i32,
    consecutive_misses: u16,
}

impl VideoSafetyController {
    pub fn new(initial_target: i32, policy: VideoSafetyPolicy) -> Self {
        Self {
            policy,
            state: VideoSafetyState::Tracking,
            current_target: initial_target
                .clamp(policy.perception.target_min, policy.perception.target_max),
            consecutive_misses: 0,
        }
    }

    pub const fn state(&self) -> VideoSafetyState {
        self.state
    }

    pub const fn current_target(&self) -> i32 {
        self.current_target
    }

    pub fn update(&mut self, input: SceneInput) -> SceneDecision {
        if matches!(input, SceneInput::Reset) {
            self.state = VideoSafetyState::Tracking;
            self.consecutive_misses = 0;
            return SceneDecision::Reset {
                target: self.current_target,
            };
        }

        if let VideoSafetyState::StoppedLatched(reason) = self.state {
            return SceneDecision::Stop {
                reason,
                newly_latched: false,
            };
        }

        let detection = match input {
            SceneInput::Detection(detection) => detection,
            SceneInput::NoDetection => return self.hold_or_stop(HoldReason::NoDetection),
            SceneInput::Reset => unreachable!(),
        };
        if detection.center_y_milli > 1000 {
            return self.hold_or_stop(HoldReason::PerceptionRejected(
                PerceptionRejectReason::InvalidCoordinate,
            ));
        }

        let bounded =
            yolo_detection_to_target(detection, self.current_target, self.policy.perception);
        let (target, class_id, confidence_milli) = match bounded {
            PerceptionDecision::Target {
                target,
                class_id,
                confidence_milli,
            } => (target, class_id, confidence_milli),
            PerceptionDecision::Reject(reason) => {
                return self.hold_or_stop(HoldReason::PerceptionRejected(reason));
            }
        };

        if self.is_hazard(detection) {
            return self.latch_stop(StopReason::HazardClass(class_id));
        }
        if !is_vehicle(class_id) {
            return self.hold_or_stop(HoldReason::NonTrackingClass(class_id));
        }

        self.current_target = target;
        self.consecutive_misses = 0;
        SceneDecision::Track {
            target,
            class_id,
            confidence_milli,
        }
    }

    fn is_hazard(&self, detection: YoloDetection) -> bool {
        if matches!(detection.class_id, COCO_KNIFE | COCO_SCISSORS) {
            return true;
        }
        detection.class_id == COCO_PERSON
            && (self.policy.danger_x_min_milli..=self.policy.danger_x_max_milli)
                .contains(&detection.center_x_milli)
            && (self.policy.danger_y_min_milli..=self.policy.danger_y_max_milli)
                .contains(&detection.center_y_milli)
    }

    fn hold_or_stop(&mut self, reason: HoldReason) -> SceneDecision {
        self.consecutive_misses = self.consecutive_misses.saturating_add(1);
        let limit = self.policy.max_consecutive_misses.max(1);
        if self.consecutive_misses >= limit {
            self.latch_stop(StopReason::TrackingLost(reason))
        } else {
            SceneDecision::Hold {
                target: self.current_target,
                reason,
                consecutive_misses: self.consecutive_misses,
            }
        }
    }

    fn latch_stop(&mut self, reason: StopReason) -> SceneDecision {
        self.state = VideoSafetyState::StoppedLatched(reason);
        SceneDecision::Stop {
            reason,
            newly_latched: true,
        }
    }
}

const fn is_vehicle(class_id: u16) -> bool {
    matches!(class_id, COCO_CAR | COCO_BUS | COCO_TRUCK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(class_id: u16, x: u16, y: u16) -> YoloDetection {
        YoloDetection {
            class_id,
            confidence_milli: 900,
            center_x_milli: x,
            center_y_milli: y,
            area_milli: 100,
        }
    }

    #[test]
    fn vehicle_motion_produces_step_limited_targets() {
        let mut controller = VideoSafetyController::new(500, VideoSafetyPolicy::task3_default());
        assert_eq!(
            controller.update(SceneInput::Detection(detection(COCO_CAR, 900, 500))),
            SceneDecision::Track {
                target: 600,
                class_id: COCO_CAR,
                confidence_milli: 900,
            }
        );
        assert_eq!(
            controller.update(SceneInput::Detection(detection(COCO_TRUCK, 100, 500))),
            SceneDecision::Track {
                target: 500,
                class_id: COCO_TRUCK,
                confidence_milli: 900,
            }
        );
    }

    #[test]
    fn person_in_danger_zone_latches_stop_until_explicit_reset() {
        let mut controller = VideoSafetyController::new(500, VideoSafetyPolicy::task3_default());
        let reason = StopReason::HazardClass(COCO_PERSON);
        assert_eq!(
            controller.update(SceneInput::Detection(detection(COCO_PERSON, 500, 700))),
            SceneDecision::Stop {
                reason,
                newly_latched: true,
            }
        );
        assert_eq!(
            controller.update(SceneInput::Detection(detection(COCO_CAR, 500, 500))),
            SceneDecision::Stop {
                reason,
                newly_latched: false,
            }
        );
        assert_eq!(
            controller.update(SceneInput::Reset),
            SceneDecision::Reset { target: 500 }
        );
        assert_eq!(controller.state(), VideoSafetyState::Tracking);
        assert!(matches!(
            controller.update(SceneInput::Detection(detection(COCO_CAR, 500, 500))),
            SceneDecision::Track { .. }
        ));
    }

    #[test]
    fn person_outside_danger_zone_does_not_immediately_stop() {
        let mut controller = VideoSafetyController::new(500, VideoSafetyPolicy::task3_default());
        assert!(matches!(
            controller.update(SceneInput::Detection(detection(COCO_PERSON, 100, 700))),
            SceneDecision::Hold {
                reason: HoldReason::NonTrackingClass(COCO_PERSON),
                consecutive_misses: 1,
                ..
            }
        ));
    }

    #[test]
    fn three_missing_frames_fail_safe_and_latch() {
        let mut controller = VideoSafetyController::new(500, VideoSafetyPolicy::task3_default());
        assert!(matches!(
            controller.update(SceneInput::NoDetection),
            SceneDecision::Hold {
                consecutive_misses: 1,
                ..
            }
        ));
        assert!(matches!(
            controller.update(SceneInput::NoDetection),
            SceneDecision::Hold {
                consecutive_misses: 2,
                ..
            }
        ));
        assert_eq!(
            controller.update(SceneInput::NoDetection),
            SceneDecision::Stop {
                reason: StopReason::TrackingLost(HoldReason::NoDetection),
                newly_latched: true,
            }
        );
    }

    #[test]
    fn rejected_hazard_observation_cannot_resume_tracking() {
        let mut controller = VideoSafetyController::new(500, VideoSafetyPolicy::task3_default());
        let mut too_small = detection(COCO_PERSON, 500, 700);
        too_small.area_milli = 9;
        for expected_miss in 1..3 {
            assert!(matches!(
                controller.update(SceneInput::Detection(too_small)),
                SceneDecision::Hold {
                    reason: HoldReason::PerceptionRejected(PerceptionRejectReason::SmallArea),
                    consecutive_misses,
                    ..
                } if consecutive_misses == expected_miss
            ));
        }
        assert!(matches!(
            controller.update(SceneInput::Detection(too_small)),
            SceneDecision::Stop {
                reason: StopReason::TrackingLost(HoldReason::PerceptionRejected(
                    PerceptionRejectReason::SmallArea
                )),
                newly_latched: true,
            }
        ));
    }

    #[test]
    fn knife_stops_even_outside_person_roi() {
        let mut controller = VideoSafetyController::new(500, VideoSafetyPolicy::task3_default());
        assert!(matches!(
            controller.update(SceneInput::Detection(detection(COCO_KNIFE, 50, 50))),
            SceneDecision::Stop {
                reason: StopReason::HazardClass(COCO_KNIFE),
                newly_latched: true,
            }
        ));
    }
}
