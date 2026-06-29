use super::*;

#[test]
fn qemu_summary_lists_passed_and_failed_cases() {
    let report = StarryQemuRunReport {
        cases: vec![
            StarryQemuCaseReport {
                name: "smoke".to_string(),
                outcome: StarryQemuCaseOutcome::Passed,
                duration: Duration::from_millis(500),
            },
            StarryQemuCaseReport {
                name: "usb".to_string(),
                outcome: StarryQemuCaseOutcome::Failed,
                duration: Duration::from_secs(2),
            },
        ],
        total_duration: Duration::from_secs(3),
    };

    let summary = render_qemu_case_summary(&report);

    assert!(summary.contains("starry qemu test summary:"));
    assert!(summary.contains("  PASS smoke (0.50s)"));
    assert!(summary.contains("  FAIL usb (2.00s)"));
    assert!(summary.contains("result: 1/2 case(s) passed"));
    assert!(summary.contains("total: 3.00s"));
}
