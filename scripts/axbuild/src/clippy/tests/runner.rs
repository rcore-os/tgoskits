use std::path::PathBuf;

use super::common::FakeCargoRunner;
use crate::clippy::{
    check::{ClippyCheck, ClippyCheckKind, ClippyDepsMode},
    runner::run_clippy_checks,
};

#[test]
fn package_failures_abort_remaining_checks() {
    let root = PathBuf::from("/tmp/workspace");
    let checks = vec![
        ClippyCheck {
            package: "alpha".into(),
            kind: ClippyCheckKind::Base,
            deps_mode: ClippyDepsMode::NoDeps,
            target: None,
            env: Vec::new(),
        },
        ClippyCheck {
            package: "alpha".into(),
            kind: ClippyCheckKind::Feature("feat-a".into()),
            deps_mode: ClippyDepsMode::NoDeps,
            target: None,
            env: Vec::new(),
        },
        ClippyCheck {
            package: "beta".into(),
            kind: ClippyCheckKind::Base,
            deps_mode: ClippyDepsMode::NoDeps,
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
