use ivcproto::{
    vision::{
        ACTUATOR_STATUS_PAYLOAD_LEN, ActuatorState, ActuatorStatus, BoundingBox,
        VISION_DECISION_PAYLOAD_LEN, VisionAction, VisionDecision, VisionEndpoint, VisionError,
    },
    wire::MessageType,
};

fn decision(frame_id: u32) -> VisionDecision {
    VisionDecision {
        requested_action: VisionAction::SortLeft,
        safe_action: VisionAction::Hold,
        detection_present: true,
        frame_id,
        captured_at_us: 1_000,
        inference_finished_at_us: 1_400,
        ttl_us: 5_000,
        class_id: 32,
        confidence_q10000: 8_591,
        region_id: 1,
        bounding_box: BoundingBox {
            left: 517,
            top: 932,
            right: 730,
            bottom: 1_151,
        },
    }
}

#[test]
fn vision_message_types_extend_the_existing_frame_version() {
    assert_eq!(
        MessageType::try_from(6).unwrap(),
        MessageType::VisionDecision
    );
    assert_eq!(
        MessageType::try_from(7).unwrap(),
        MessageType::ActuatorStatus
    );
}

#[test]
fn vision_decision_has_c_compatible_golden_bytes() {
    let encoded = decision(0x0102_0304).encode().unwrap();

    assert_eq!(encoded.len(), VISION_DECISION_PAYLOAD_LEN);
    assert_eq!(
        encoded,
        [
            1, 1, 0, 1, 4, 3, 2, 1, 0xe8, 3, 0, 0, 0, 0, 0, 0, 0x78, 5, 0, 0, 0, 0, 0, 0, 0x88,
            0x13, 0, 0, 32, 0, 0x8f, 0x21, 1, 0, 0, 0, 5, 2, 0xa4, 3, 0xda, 2, 0x7f, 4,
        ]
    );
    assert_eq!(
        VisionDecision::decode(&encoded).unwrap(),
        decision(0x0102_0304)
    );
}

#[test]
fn actuator_status_has_c_compatible_golden_bytes() {
    let status = ActuatorStatus {
        state: ActuatorState::Applied,
        requested_action: VisionAction::SortLeft,
        actual_action: VisionAction::SortLeft,
        frame_id: 0x0102_0304,
        applied_sequence: 7,
        decision_age_at_send_us: 600,
        local_apply_latency_us: 25,
        executed_at_us: 0x1112_1314_1516_1718,
        fault_code: 0,
    };
    let encoded = status.encode().unwrap();

    assert_eq!(encoded.len(), ACTUATOR_STATUS_PAYLOAD_LEN);
    assert_eq!(
        encoded,
        [
            1, 1, 1, 1, 4, 3, 2, 1, 7, 0, 0, 0, 0x58, 2, 0, 0, 25, 0, 0, 0, 0x18, 0x17, 0x16, 0x15,
            0x14, 0x13, 0x12, 0x11, 0, 0, 0, 0,
        ]
    );
    assert_eq!(ActuatorStatus::decode(&encoded).unwrap(), status);
}

#[test]
fn unsafe_safe_action_and_inconsistent_detection_are_rejected() {
    let mut unsafe_decision = decision(1);
    unsafe_decision.safe_action = VisionAction::SortRight;
    assert_eq!(
        unsafe_decision.encode(),
        Err(VisionError::UnsafeFallbackAction)
    );

    let mut missing_detection = decision(1);
    missing_detection.detection_present = false;
    assert_eq!(
        missing_detection.encode(),
        Err(VisionError::InconsistentDetection)
    );
}

#[test]
fn endpoint_rejects_expired_and_replayed_frames_and_times_out_safe() {
    let mut endpoint = VisionEndpoint::new(500_000);
    let first = decision(1);
    let applied = endpoint.apply(1, first, 2_000, 10_000).unwrap();
    assert_eq!(applied.actual_action, VisionAction::SortLeft);
    assert_eq!(
        endpoint.apply(2, first, 2_100, 10_100),
        Err(VisionError::ReplayedFrame(1))
    );

    let mut expired = decision(2);
    expired.ttl_us = 500;
    assert_eq!(
        endpoint.apply(2, expired, 2_000, 10_100),
        Err(VisionError::ExpiredDecision)
    );

    assert!(endpoint.check_timeout(510_000).is_none());
    let fallback = endpoint.check_timeout(510_001).unwrap();
    assert_eq!(fallback.state, ActuatorState::SafeFallback);
    assert_eq!(fallback.actual_action, VisionAction::Hold);
    assert!(endpoint.check_timeout(510_002).is_none());
}
