use crate::clippy::report::{ClippyRunReport, PackageRunReport};

#[test]
fn report_distinguishes_completed_failed_and_unfinished_packages() {
    let report = ClippyRunReport {
        total_checks: 5,
        passed_checks: 3,
        packages: vec![
            PackageRunReport {
                package: "alpha".into(),
                planned_checks: 2,
                total_checks: 2,
                failed_checks: vec!["alpha (feature: feat-a)".into()],
            },
            PackageRunReport {
                package: "beta".into(),
                planned_checks: 1,
                total_checks: 1,
                failed_checks: Vec::new(),
            },
            PackageRunReport {
                package: "gamma".into(),
                planned_checks: 2,
                total_checks: 1,
                failed_checks: Vec::new(),
            },
        ],
    };

    assert_eq!(report.failed_packages(), vec!["alpha"]);
    assert_eq!(report.passed_packages(), vec!["beta"]);
    assert_eq!(report.skipped_packages(), vec!["gamma"]);
}
