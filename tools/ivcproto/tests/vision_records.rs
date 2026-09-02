use ivcproto::{
    vision::{BoundingBox, VisionAction},
    vision_records::{VisionRecordError, parse_decision_record},
};

#[test]
fn parses_machine_record_emitted_by_the_rknn_runner() {
    let record = "VISION_DECISION_RECORD version=1 frame_id=1 captured_at_us=1000 \
                  inference_finished_at_us=1400 ttl_us=5000000 requested_action=right \
                  safe_action=hold detection_present=1 class_id=32 confidence_q10000=7081 \
                  region_id=2 left=479 top=706 right=773 bottom=1010";
    let decision = parse_decision_record(record).unwrap().unwrap();

    assert_eq!(decision.frame_id, 1);
    assert_eq!(decision.requested_action, VisionAction::SortRight);
    assert_eq!(decision.safe_action, VisionAction::Hold);
    assert_eq!(decision.class_id, 32);
    assert_eq!(decision.confidence_q10000, 7081);
    assert_eq!(
        decision.bounding_box,
        BoundingBox {
            left: 479,
            top: 706,
            right: 773,
            bottom: 1010,
        }
    );
}

#[test]
fn ignores_unrelated_runner_output_and_rejects_record_schema_drift() {
    assert_eq!(
        parse_decision_record("bench-rknn: init success").unwrap(),
        None
    );
    assert_eq!(
        parse_decision_record("VISION_DECISION_RECORD frame_id=1 version=1 captured_at_us=1000"),
        Err(VisionRecordError::InvalidFieldCount { actual: 3 })
    );
}
