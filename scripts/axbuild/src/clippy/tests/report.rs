use crate::clippy::report::{ClippyRunReport, PackageRunReport};

#[test]
fn report_tracks_passing_and_failing_packages_for_mixed_runs() {
    let report = ClippyRunReport {
        total_checks: 3,
        passed_checks: 2,
        packages: vec![
            PackageRunReport {
                package: "alpha".into(),
                total_checks: 2,
                failed_checks: vec!["alpha (feature: feat-a)".into()],
            },
            PackageRunReport {
                package: "beta".into(),
                total_checks: 1,
                failed_checks: Vec::new(),
            },
        ],
    };

    assert_eq!(report.failed_packages(), vec!["alpha"]);
    assert_eq!(report.passed_packages(), vec!["beta"]);
}
