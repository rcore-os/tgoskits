use super::common::{expand, metadata_for_packages, metadata_with_resolve, pkg, pkg_with_metadata};
use crate::clippy::{
    AXSTD_STD_CLIPPY_FEATURES, AXSTD_STD_DEFAULT_FEATURE, AXSTD_STD_PACKAGE,
    check::{ClippyCheck, ClippyCheckKind, ClippyDepsMode},
    configurations::package_clippy_configurations,
    selection::{SelectedClippyPackage, incremental_clippy_selections},
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
            },
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Feature("feat-a".into()),
                deps_mode: ClippyDepsMode::NoDeps,
                target: None,
                env: Vec::new(),
            },
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Feature("feat-b".into()),
                deps_mode: ClippyDepsMode::NoDeps,
                target: None,
                env: Vec::new(),
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
fn with_deps_incremental_frontier_expands_only_base_checks() {
    let packages = vec![
        pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("feat-a", &[])],
            None,
        ),
        pkg(
            "gamma",
            "gamma 0.1.0 (path+file:///tmp/gamma)",
            &[("feat-b", &[])],
            None,
        ),
    ];
    let metadata =
        metadata_with_resolve(packages.clone(), &[("alpha", &[]), ("gamma", &["alpha"])]);
    let selections = incremental_clippy_selections(
        vec!["alpha".into()],
        vec!["alpha".into(), "gamma".into()],
        &metadata,
        &packages,
    )
    .into_iter()
    .map(|(name, deps_mode)| {
        let package = packages
            .iter()
            .find(|package| package.name == name)
            .cloned()
            .unwrap();
        crate::clippy::selection::SelectedClippyPackage { package, deps_mode }
    })
    .collect::<Vec<_>>();

    let checks = crate::clippy::expand::expand_clippy_checks(&selections, &metadata).unwrap();

    assert_eq!(
        checks
            .into_iter()
            .map(|check| check.label())
            .collect::<Vec<_>>(),
        vec!["alpha (base)", "alpha (feature: feat-a)", "gamma (base)"]
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
        &[("irq", &[])],
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
}

#[test]
fn ax_hal_target_only_features_are_skipped_for_host_clippy() {
    let checks = expand(&[pkg(
        "ax-hal",
        "ax-hal 0.1.0 (path+file:///tmp/ax-hal)",
        &[("irq", &[])],
        None,
    )]);

    assert!(checks.iter().any(|check| {
        matches!(&check.kind, ClippyCheckKind::Feature(feature) if feature == "irq")
    }));
}

#[test]
fn ax_hal_platform_feature_forwards_are_filtered_by_target_arch() {
    let checks = expand(&[pkg(
        "platform-forwarder",
        "platform-forwarder 0.1.0 (path+file:///tmp/platform-forwarder)",
        &[("irq", &["ax-hal/irq"])],
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

#[test]
fn package_clippy_configurations_expand_target_feature_sets() {
    let checks = expand(&[pkg_with_metadata(
        "arm-perf-fixture",
        "arm-perf-fixture 0.1.0 (path+file:///tmp/arm-perf-fixture)",
        &[("default", &["dynamic-debug"]), ("dynamic-debug", &[])],
        serde_json::json!({
            "clippy": {
                "configurations": [{
                    "name": "aarch64-system",
                    "target": "aarch64-unknown-none-softfloat",
                    "features": [
                        "ax-runtime/display",
                        "input",
                        "smp",
                    ],
                    "env": {
                        "AX_ARCH": "aarch64",
                        "AX_LOG": "warn",
                        "AX_TARGET": "aarch64-unknown-none-softfloat",
                        "SMP": "4",
                    },
                }],
            },
        }),
    )]);

    let target_configuration = checks
        .iter()
        .find(|check| {
            check.label()
                == "arm-perf-fixture (configuration: aarch64-system, features: \
                    ax-runtime/display,input,smp, target: aarch64-unknown-none-softfloat)"
        })
        .expect("aarch64 system configuration should be planned");

    assert_eq!(
        target_configuration.cargo_args(),
        vec![
            "clippy",
            "--no-deps",
            "-p",
            "arm-perf-fixture",
            "--features",
            "ax-runtime/display,input,smp",
            "--target",
            "aarch64-unknown-none-softfloat",
            "--",
            "-D",
            "warnings",
        ]
    );
    assert_eq!(
        target_configuration.env,
        vec![
            ("AX_ARCH".into(), "aarch64".into()),
            ("AX_LOG".into(), "warn".into()),
            ("AX_TARGET".into(), "aarch64-unknown-none-softfloat".into()),
            ("SMP".into(), "4".into()),
        ]
    );
}

#[test]
fn with_deps_selection_does_not_expand_package_clippy_configurations() {
    let package = pkg_with_metadata(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[],
        serde_json::json!({
            "clippy": {
                "configurations": [{
                    "name": "aarch64-system",
                    "target": "aarch64-unknown-none-softfloat",
                }],
            },
        }),
    );
    let metadata = metadata_for_packages(core::slice::from_ref(&package));
    let checks = crate::clippy::expand::expand_clippy_checks(
        &[SelectedClippyPackage {
            package,
            deps_mode: ClippyDepsMode::WithDeps,
        }],
        &metadata,
    )
    .unwrap();

    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].label(), "alpha (base)");
}

#[test]
fn duplicate_package_clippy_configuration_names_are_rejected() {
    let package = pkg_with_metadata(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[],
        serde_json::json!({
            "clippy": {
                "configurations": [
                    {
                        "name": "aarch64-system",
                        "target": "aarch64-unknown-none-softfloat",
                    },
                    {
                        "name": "aarch64-system",
                        "target": "aarch64-unknown-none-softfloat",
                    },
                ],
            },
        }),
    );

    let err = package_clippy_configurations(&package).unwrap_err();

    assert_eq!(
        err.to_string(),
        "duplicate clippy configuration `aarch64-system` for `alpha`"
    );
}

#[test]
fn starry_aarch64_clippy_configurations_match_qemu_builds() {
    let workspace_root = crate::context::find_workspace_root();
    let manifest: StarryKernelManifest = toml::from_str(
        &std::fs::read_to_string(workspace_root.join("os/StarryOS/kernel/Cargo.toml")).unwrap(),
    )
    .unwrap();

    for (name, relative_build_path) in [
        (
            "aarch64-system",
            "test-suit/starryos/qemu/build-aarch64-unknown-none-softfloat.toml",
        ),
        (
            "aarch64-system-rga",
            "test-suit/starryos/qemu-rga/build-aarch64-unknown-none-softfloat.toml",
        ),
    ] {
        let build: StarryBuildConfiguration = toml::from_str(
            &std::fs::read_to_string(workspace_root.join(relative_build_path)).unwrap(),
        )
        .unwrap();
        let configuration = manifest
            .package
            .metadata
            .clippy
            .configurations
            .iter()
            .find(|configuration| configuration.name == name)
            .unwrap();
        let mut expected_features = build.features;
        expected_features.push("smp".into());
        expected_features.sort();
        expected_features.dedup();

        assert_eq!(configuration.features, expected_features);
        assert_eq!(configuration.target, build.target);
        assert_eq!(configuration.env.get("AX_ARCH"), Some(&"aarch64".into()));
        assert_eq!(
            configuration.env.get("AX_TARGET"),
            Some(&configuration.target)
        );
        assert_eq!(
            configuration.env.get("AX_LOG"),
            Some(&build.log.to_ascii_lowercase())
        );
        assert_eq!(
            configuration.env.get("SMP"),
            Some(&build.max_cpu_num.to_string())
        );
    }
}

#[derive(serde::Deserialize)]
struct StarryKernelManifest {
    package: StarryKernelPackage,
}

#[derive(serde::Deserialize)]
struct StarryKernelPackage {
    metadata: StarryKernelMetadata,
}

#[derive(serde::Deserialize)]
struct StarryKernelMetadata {
    clippy: StarryClippyMetadata,
}

#[derive(serde::Deserialize)]
struct StarryClippyMetadata {
    configurations: Vec<StarryClippyConfiguration>,
}

#[derive(serde::Deserialize)]
struct StarryClippyConfiguration {
    name: String,
    target: String,
    features: Vec<String>,
    env: std::collections::BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
struct StarryBuildConfiguration {
    features: Vec<String>,
    max_cpu_num: usize,
    log: String,
    target: String,
}
