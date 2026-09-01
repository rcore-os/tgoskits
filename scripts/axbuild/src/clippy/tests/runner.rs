use std::path::PathBuf;

use super::common::FakeCargoRunner;
use crate::clippy::{
    check::{ClippyCheck, ClippyCheckKind},
    runner::run_clippy_checks,
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

    let err = run_clippy_checks(&mut runner, &root, &checks).unwrap_err();

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

    let error = run_clippy_checks(&mut runner, root.path(), &[check]).unwrap_err();

    assert!(
        format!("{error:#}").contains("unapproved future-incompatible package"),
        "{error:#}"
    );
}
