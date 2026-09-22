use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
};

use super::common::FakeCargoRunner;
use crate::clippy::{
    check::{ClippyCheck, ClippyCheckKind},
    runner::{run_clippy_checks, run_parallel_checks},
};

#[test]
fn package_failures_abort_remaining_checks() {
    let root = PathBuf::from("/tmp/workspace");
    let checks = vec![
        ClippyCheck {
            package: "alpha".into(),
            kind: ClippyCheckKind::Base,
            target: None,
            env: Vec::new(),
        },
        ClippyCheck {
            package: "alpha".into(),
            kind: ClippyCheckKind::Feature("feat-a".into()),
            target: None,
            env: Vec::new(),
        },
        ClippyCheck {
            package: "beta".into(),
            kind: ClippyCheckKind::Base,
            target: None,
            env: Vec::new(),
        },
    ];
    let mut runner = FakeCargoRunner::new(&[
        (checks[0].clone(), true),
        (checks[1].clone(), false),
        (checks[2].clone(), true),
    ]);

    let err = run_clippy_checks(&mut runner, &root, &root.join("target"), &checks).unwrap_err();

    assert_eq!(
        err.to_string(),
        "clippy failed for alpha (feature: feat-a): aborting (fail-fast, 1 check(s) remaining)"
    );
    assert_eq!(
        runner.invocations,
        vec![
            (root.clone(), checks[0].clone()),
            (root.clone(), checks[1].clone()),
        ]
    );
}

#[test]
fn parallel_checks_run_concurrently_and_collect_failures() {
    let root = tempfile::tempdir().unwrap();
    let checks = (0..2)
        .map(|index| ClippyCheck {
            package: format!("package-{index}"),
            kind: ClippyCheckKind::Base,
            target: None,
            env: Vec::new(),
        })
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(2));
    let report = run_parallel_checks(root.path(), root.path(), &checks, 2, |_, target, check| {
        assert!(target.ends_with("worker-0") || target.ends_with("worker-1"));
        barrier.wait();
        Ok((check.package != "package-0", Vec::new(), Vec::new()))
    })
    .unwrap();

    assert_eq!(report.passed_checks, 1);
    assert_eq!(report.failed_packages(), vec!["package-0"]);
    assert_eq!(
        report
            .packages
            .iter()
            .map(|p| p.total_checks)
            .sum::<usize>(),
        2
    );
}

#[test]
fn parallel_fail_fast_does_not_dispatch_remaining_checks() {
    let root = tempfile::tempdir().unwrap();
    let checks = (0..3)
        .map(|index| ClippyCheck {
            package: format!("package-{index}"),
            kind: ClippyCheckKind::Base,
            target: None,
            env: Vec::new(),
        })
        .collect::<Vec<_>>();
    // One worker exercises the same dispatch boundary without scheduling races.
    let report = run_parallel_checks(root.path(), root.path(), &checks, 1, |_, _, _| {
        Ok((false, Vec::new(), Vec::new()))
    })
    .unwrap();

    assert_eq!(report.failed_packages(), vec!["package-0"]);
    assert!(report.passed_packages().is_empty());
    assert_eq!(report.skipped_packages(), vec!["package-1", "package-2"]);
    assert_eq!(
        report
            .packages
            .iter()
            .map(|p| p.total_checks)
            .sum::<usize>(),
        1
    );
}

#[test]
fn aarch64_clippy_rejects_unapproved_current_future_incompat_report() {
    let root = tempfile::tempdir().unwrap();
    let check = ClippyCheck {
        package: "starry-kernel".into(),
        kind: ClippyCheckKind::Base,
        target: Some("aarch64-unknown-none-softfloat".into()),
        env: Vec::new(),
    };
    let report = serde_json::json!({
        "version": 0,
        "next_id": 2,
        "reports": [{
            "id": 1,
            "suggestion_message": "other@1.0.0",
            "per_package": {
                "other@1.0.0": "unexpected diagnostic",
            },
        }],
    });
    let mut runner =
        FakeCargoRunner::new(&[(check.clone(), true)]).with_future_incompat_report(report);

    let error = run_clippy_checks(
        &mut runner,
        root.path(),
        &root.path().join("target"),
        &[check],
    )
    .unwrap_err();

    assert!(
        format!("{error:#}").contains("unapproved future-incompatible package"),
        "{error:#}"
    );
}
