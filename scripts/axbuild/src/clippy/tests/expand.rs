use super::common::{expand, metadata_with_resolve, pkg, pkg_with_metadata};
use crate::clippy::{
    AXSTD_STD_CLIPPY_FEATURES, AXSTD_STD_DEFAULT_FEATURE, AXSTD_STD_PACKAGE,
    check::{ClippyCheck, ClippyCheckKind, ClippyDepsMode},
    selection::incremental_clippy_selections,
    targets::docs_rs_targets,
};

#[test]
fn feature_expansion_ignores_default() {
    let packages = vec![pkg(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[("default", &["feat-a"]), ("feat-b", &[]), ("feat-a", &[])],
        None,
    )];

    let checks = expand(&packages);

    assert_eq!(
        checks,
        vec![
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Base,
                deps_mode: ClippyDepsMode::NoDeps,
                target: None,
                env: Vec::new(),
                axconfig_override: None,
            },
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Feature("feat-a".into()),
                deps_mode: ClippyDepsMode::NoDeps,
                target: None,
                env: Vec::new(),
                axconfig_override: None,
            },
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Feature("feat-b".into()),
                deps_mode: ClippyDepsMode::NoDeps,
                target: None,
                env: Vec::new(),
                axconfig_override: None,
            },
        ]
    );
}

#[test]
fn feature_expansion_is_deterministic() {
    let packages = vec![
        pkg(
            "beta",
            "beta 0.1.0 (path+file:///tmp/beta)",
            &[("zeta", &[]), ("alpha", &[])],
            None,
        ),
        pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("middle", &[]), ("default", &[])],
            None,
        ),
    ];

    let checks = expand(&packages);

    assert_eq!(
        checks
            .into_iter()
            .map(|check| check.label())
            .collect::<Vec<_>>(),
        vec![
            "beta (base)",
            "beta (feature: alpha)",
            "beta (feature: zeta)",
            "alpha (base)",
            "alpha (feature: middle)",
        ]
    );
}

#[test]
fn incremental_selection_keeps_runnable_top_levels_when_some_are_skipped() {
    let packages = vec![
        pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
        pkg("axvm", "axvm 0.1.0 (path+file:///tmp/axvm)", &[], None),
        pkg(
            "axvisor",
            "axvisor 0.1.0 (path+file:///tmp/axvisor)",
            &[],
            None,
        ),
        pkg("app", "app 0.1.0 (path+file:///tmp/app)", &[], None),
    ];
    let metadata = metadata_with_resolve(
        packages.clone(),
        &[
            ("alpha", &[]),
            ("axvm", &["alpha"]),
            ("axvisor", &["axvm"]),
            ("app", &["axvm"]),
        ],
    );

    let selected = incremental_clippy_selections(
        vec!["alpha".into()],
        vec![
            "alpha".into(),
            "axvm".into(),
            "axvisor".into(),
            "app".into(),
        ],
        &metadata,
        &packages,
    );

    assert_eq!(
        selected,
        vec![
            ("alpha".into(), ClippyDepsMode::NoDeps),
            ("app".into(), ClippyDepsMode::WithDeps),
        ]
    );
}

#[test]
fn incremental_selection_falls_back_when_all_top_levels_are_skipped() {
    let packages = vec![
        pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
        pkg("axvm", "axvm 0.1.0 (path+file:///tmp/axvm)", &[], None),
        pkg(
            "axvisor",
            "axvisor 0.1.0 (path+file:///tmp/axvisor)",
            &[],
            None,
        ),
    ];
    let metadata = metadata_with_resolve(
        packages.clone(),
        &[("alpha", &[]), ("axvm", &["alpha"]), ("axvisor", &["axvm"])],
    );

    let selected = incremental_clippy_selections(
        vec!["alpha".into()],
        vec!["alpha".into(), "axvm".into(), "axvisor".into()],
        &metadata,
        &packages,
    );

    assert_eq!(
        selected,
        vec![
            ("alpha".into(), ClippyDepsMode::NoDeps),
            ("axvm".into(), ClippyDepsMode::WithDeps),
        ]
    );
}

#[test]
fn incremental_selection_recomputes_frontier_around_skipped_top_level() {
    // `shared` is depended on by both a runnable top-level (`app`) and the
    // skipped top-level (`axvisor`). `axvm` sits only under `axvisor`, so
    // merely dropping skipped top-levels would leave `axvm` unlinted. The
    // frontier must be recomputed over `affected \ skipped` so `axvm` is
    // re-promoted to a runnable with-deps root.
    let packages = vec![
        pkg(
            "shared",
            "shared 0.1.0 (path+file:///tmp/shared)",
            &[],
            None,
        ),
        pkg("app", "app 0.1.0 (path+file:///tmp/app)", &[], None),
        pkg("axvm", "axvm 0.1.0 (path+file:///tmp/axvm)", &[], None),
        pkg(
            "axvisor",
            "axvisor 0.1.0 (path+file:///tmp/axvisor)",
            &[],
            None,
        ),
    ];
    let metadata = metadata_with_resolve(
        packages.clone(),
        &[
            ("shared", &[]),
            ("app", &["shared"]),
            ("axvm", &["shared"]),
            ("axvisor", &["axvm"]),
        ],
    );

    let selected = incremental_clippy_selections(
        vec!["shared".into()],
        vec![
            "app".into(),
            "axvm".into(),
            "axvisor".into(),
            "shared".into(),
        ],
        &metadata,
        &packages,
    );

    assert_eq!(
        selected,
        vec![
            ("shared".into(), ClippyDepsMode::NoDeps),
            ("app".into(), ClippyDepsMode::WithDeps),
            ("axvm".into(), ClippyDepsMode::WithDeps),
        ]
    );
}

#[test]
fn incremental_selection_uses_natural_frontier_when_nothing_is_skipped() {
    let packages = vec![
        pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
        pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
        pkg("gamma", "gamma 0.1.0 (path+file:///tmp/gamma)", &[], None),
    ];
    let metadata = metadata_with_resolve(
        packages.clone(),
        &[("alpha", &[]), ("beta", &["alpha"]), ("gamma", &["beta"])],
    );

    let selected = incremental_clippy_selections(
        vec!["alpha".into()],
        vec!["alpha".into(), "beta".into(), "gamma".into()],
        &metadata,
        &packages,
    );

    assert_eq!(
        selected,
        vec![
            ("alpha".into(), ClippyDepsMode::NoDeps),
            ("gamma".into(), ClippyDepsMode::WithDeps),
        ]
    );
}

#[test]
fn incremental_selection_keeps_changed_unsupported_crate_for_shared_skip_handling() {
    // Editing an unsupported crate's own source (e.g. `axvisor`) keeps it in
    // the `changed` selection instead of dropping it here; the shared
    // `skip_unsupported_packages` pass then removes it and prints the skip
    // message, matching `--all`/default behaviour.
    let packages = vec![pkg(
        "axvisor",
        "axvisor 0.1.0 (path+file:///tmp/axvisor)",
        &[],
        None,
    )];
    let metadata = metadata_with_resolve(packages.clone(), &[("axvisor", &[])]);

    let selected = incremental_clippy_selections(
        vec!["axvisor".into()],
        vec!["axvisor".into()],
        &metadata,
        &packages,
    );

    assert_eq!(selected, vec![("axvisor".into(), ClippyDepsMode::NoDeps)]);
}

#[test]
fn with_deps_check_omits_no_deps_flag() {
    let check = ClippyCheck {
        package: "alpha".into(),
        kind: ClippyCheckKind::Base,
        deps_mode: ClippyDepsMode::WithDeps,
        target: None,
        env: Vec::new(),
        axconfig_override: None,
    };

    assert_eq!(
        check.cargo_args(),
        vec!["clippy", "-p", "alpha", "--", "-D", "warnings"]
    );
}

#[test]
fn axstd_default_feature_no_deps_check_keeps_no_deps_flag() {
    let check = ClippyCheck {
        package: AXSTD_STD_PACKAGE.into(),
        kind: ClippyCheckKind::Feature(AXSTD_STD_DEFAULT_FEATURE.into()),
        deps_mode: ClippyDepsMode::NoDeps,
        target: None,
        env: Vec::new(),
        axconfig_override: None,
    };

    assert_eq!(
        check.cargo_args(),
        vec![
            "clippy",
            "--no-deps",
            "-p",
            AXSTD_STD_PACKAGE,
            "--no-default-features",
            "--features",
            AXSTD_STD_CLIPPY_FEATURES,
            "--",
            "-D",
            "warnings",
        ]
    );
}

#[test]
fn package_without_features_yields_only_base_check() {
    let checks = expand(&[pkg(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[],
        None,
    )]);

    assert_eq!(
        checks,
        vec![ClippyCheck {
            package: "alpha".into(),
            kind: ClippyCheckKind::Base,
            deps_mode: ClippyDepsMode::NoDeps,
            target: None,
            env: Vec::new(),
            axconfig_override: None,
        }]
    );
}

#[test]
fn package_with_features_yields_base_plus_each_feature() {
    let checks = expand(&[pkg(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[("b", &[]), ("a", &[])],
        None,
    )]);

    assert_eq!(checks.len(), 3);
    assert_eq!(
        checks[0].cargo_args(),
        vec!["clippy", "--no-deps", "-p", "alpha", "--", "-D", "warnings"]
    );
    assert_eq!(
        checks[1].cargo_args(),
        vec![
            "clippy",
            "--no-deps",
            "-p",
            "alpha",
            "--no-default-features",
            "--features",
            "a",
            "--",
            "-D",
            "warnings",
        ]
    );
    assert_eq!(
        checks[2].cargo_args(),
        vec![
            "clippy",
            "--no-deps",
            "-p",
            "alpha",
            "--no-default-features",
            "--features",
            "b",
            "--",
            "-D",
            "warnings",
        ]
    );
}

#[test]
fn docs_rs_targets_expand_base_and_feature_checks() {
    let checks = expand(&[pkg(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[("b", &[]), ("a", &[])],
        Some(&["riscv64gc-unknown-none-elf"]),
    )]);

    assert_eq!(checks.len(), 3);
    assert_eq!(
        checks[0].cargo_args(),
        vec![
            "clippy",
            "--no-deps",
            "-p",
            "alpha",
            "--target",
            "riscv64gc-unknown-none-elf",
            "--",
            "-D",
            "warnings",
        ]
    );
    assert_eq!(
        checks[1].cargo_args(),
        vec![
            "clippy",
            "--no-deps",
            "-p",
            "alpha",
            "--no-default-features",
            "--features",
            "a",
            "--target",
            "riscv64gc-unknown-none-elf",
            "--",
            "-D",
            "warnings",
        ]
    );
    assert_eq!(
        checks[2].label(),
        "alpha (feature: b, target: riscv64gc-unknown-none-elf)"
    );
}

#[test]
fn ax_hal_platform_features_are_filtered_by_target_arch() {
    let checks = expand(&[pkg(
        "ax-hal",
        "ax-hal 0.1.0 (path+file:///tmp/ax-hal)",
        &[
            ("irq", &[]),
            ("loongarch64-qemu-virt", &[]),
            ("riscv64-sg2002", &[]),
        ],
        Some(&["loongarch64-unknown-none", "riscv64gc-unknown-none-elf"]),
    )]);

    let has_feature_on_target = |feature: &str, target: &str| {
        checks.iter().any(|check| {
            matches!(&check.kind, ClippyCheckKind::Feature(check_feature) if check_feature == feature)
                && check.target.as_deref() == Some(target)
        })
    };

    assert!(has_feature_on_target(
        "irq",
        "loongarch64-unknown-none-softfloat"
    ));
    assert!(has_feature_on_target("irq", "riscv64gc-unknown-none-elf"));
    assert!(has_feature_on_target(
        "loongarch64-qemu-virt",
        "loongarch64-unknown-none-softfloat"
    ));
    assert!(!has_feature_on_target(
        "loongarch64-qemu-virt",
        "riscv64gc-unknown-none-elf"
    ));
    assert!(has_feature_on_target(
        "riscv64-sg2002",
        "riscv64gc-unknown-none-elf"
    ));
    assert!(!has_feature_on_target(
        "riscv64-sg2002",
        "loongarch64-unknown-none-softfloat"
    ));
}

#[test]
fn ax_hal_target_only_features_are_skipped_for_host_clippy() {
    let checks = expand(&[pkg(
        "ax-hal",
        "ax-hal 0.1.0 (path+file:///tmp/ax-hal)",
        &[("irq", &[]), ("plat-dyn", &[]), ("riscv64-sg2002", &[])],
        None,
    )]);

    assert!(checks.iter().any(|check| {
        matches!(&check.kind, ClippyCheckKind::Feature(feature) if feature == "irq")
    }));
    assert!(!checks.iter().any(|check| {
        matches!(&check.kind, ClippyCheckKind::Feature(feature) if feature == "plat-dyn")
    }));
    assert!(!checks.iter().any(|check| {
        matches!(&check.kind, ClippyCheckKind::Feature(feature) if feature == "riscv64-sg2002")
    }));
}

#[test]
fn ax_hal_platform_feature_forwards_are_filtered_by_target_arch() {
    let checks = expand(&[pkg(
        "platform-forwarder",
        "platform-forwarder 0.1.0 (path+file:///tmp/platform-forwarder)",
        &[
            ("irq", &["ax-hal/irq"]),
            ("loongarch64-qemu-virt", &["ax-hal/loongarch64-qemu-virt"]),
            ("riscv64-sg2002", &["ax-hal/riscv64-sg2002"]),
        ],
        Some(&["loongarch64-unknown-none", "riscv64gc-unknown-none-elf"]),
    )]);

    let has_feature_on_target = |feature: &str, target: &str| {
        checks.iter().any(|check| {
            matches!(&check.kind, ClippyCheckKind::Feature(check_feature) if check_feature == feature)
                && check.target.as_deref() == Some(target)
        })
    };

    assert!(has_feature_on_target(
        "irq",
        "loongarch64-unknown-none-softfloat"
    ));
    assert!(has_feature_on_target("irq", "riscv64gc-unknown-none-elf"));
    assert!(has_feature_on_target(
        "loongarch64-qemu-virt",
        "loongarch64-unknown-none-softfloat"
    ));
    assert!(!has_feature_on_target(
        "loongarch64-qemu-virt",
        "riscv64gc-unknown-none-elf"
    ));
    assert!(has_feature_on_target(
        "riscv64-sg2002",
        "riscv64gc-unknown-none-elf"
    ));
    assert!(!has_feature_on_target(
        "riscv64-sg2002",
        "loongarch64-unknown-none-softfloat"
    ));
}

#[test]
fn nested_docs_rs_targets_expand_base_checks() {
    let checks = expand(&[pkg_with_metadata(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[],
        serde_json::json!({
            "docs": {
                "rs": {
                    "targets": ["aarch64-unknown-none"],
                },
            },
        }),
    )]);

    assert_eq!(
        checks[0].cargo_args(),
        vec![
            "clippy",
            "--no-deps",
            "-p",
            "alpha",
            "--target",
            "aarch64-unknown-none-softfloat",
            "--",
            "-D",
            "warnings",
        ]
    );
}

#[test]
fn docs_rs_targets_are_normalized_to_workspace_toolchain_targets() {
    let checks = expand(&[pkg(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[],
        Some(&["loongarch64-unknown-none"]),
    )]);

    assert_eq!(
        checks[0].label(),
        "alpha (base, target: loongarch64-unknown-none-softfloat)"
    );
}

#[test]
fn docs_rs_targets_are_sorted_and_deduplicated() {
    let checks = expand(&[pkg(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[("feat", &[])],
        Some(&[
            "riscv64gc-unknown-none-elf",
            "aarch64-unknown-none-softfloat",
            "riscv64gc-unknown-none-elf",
        ]),
    )]);

    assert_eq!(
        checks
            .into_iter()
            .map(|check| check.label())
            .collect::<Vec<_>>(),
        vec![
            "alpha (base, target: aarch64-unknown-none-softfloat)",
            "alpha (feature: feat, target: aarch64-unknown-none-softfloat)",
            "alpha (base, target: riscv64gc-unknown-none-elf)",
            "alpha (feature: feat, target: riscv64gc-unknown-none-elf)",
        ]
    );
}

#[test]
fn empty_docs_rs_targets_fall_back_to_host_clippy() {
    let package = pkg(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[],
        Some(&[]),
    );

    assert!(docs_rs_targets(&package).is_empty());
    assert_eq!(
        expand(&[package])[0].cargo_args(),
        vec!["clippy", "--no-deps", "-p", "alpha", "--", "-D", "warnings"]
    );
}
