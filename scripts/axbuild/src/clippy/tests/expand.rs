use super::common::{expand, metadata_for_packages, metadata_with_resolve, pkg, pkg_with_metadata};
use crate::clippy::{
    AXSTD_STD_CLIPPY_FEATURES, AXSTD_STD_DEFAULT_FEATURE, AXSTD_STD_PACKAGE,
    check::{ClippyCheck, ClippyCheckKind},
    configurations::package_clippy_configurations,
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
                package: "alpha".into(),
                kind: ClippyCheckKind::Feature("feat-b".into()),
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
fn host_test_feature_lints_test_targets() {
    let checks = expand(&[pkg(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[("host-test", &[])],
        None,
    )]);
    let host_test = checks
        .iter()
        .find(|check| check.label() == "alpha (feature: host-test)")
        .expect("host-test feature check should be planned");

    assert_eq!(
        host_test.cargo_args(),
        vec![
            "clippy",
            "--no-deps",
            "-p",
            "alpha",
            "--tests",
            "--no-default-features",
            "--features",
            "host-test",
            "--",
            "-D",
            "warnings",
        ]
    );
}

#[test]
fn host_test_feature_uses_host_target_outside_docs_target_matrix() {
    let checks = expand(&[pkg(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[("host-test", &[]), ("platform", &[])],
        Some(&["aarch64-unknown-none-softfloat"]),
    )]);
    let host_test_checks = checks
        .iter()
        .filter(|check| check.label().contains("feature: host-test"))
        .collect::<Vec<_>>();

    assert_eq!(host_test_checks[0].label(), "alpha (feature: host-test)");
    assert!(
        !host_test_checks[0]
            .cargo_args()
            .contains(&"--target".into())
    );
}

#[test]
fn host_test_feature_alias_uses_host_target_outside_docs_target_matrix() {
    let checks = expand(&[pkg(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[
            ("host-test", &[]),
            ("platform", &[]),
            ("test", &["host-test"]),
        ],
        Some(&["aarch64-unknown-none-softfloat"]),
    )]);
    let test_checks = checks
        .iter()
        .filter(|check| check.label().contains("feature: test"))
        .collect::<Vec<_>>();

    assert_eq!(test_checks[0].label(), "alpha (feature: test)");
    assert!(!test_checks[0].cargo_args().contains(&"--target".into()));
}

#[test]
fn clippy_uses_shared_bare_target_specs_for_all_architectures() {
    for target in [
        "x86_64-unknown-none",
        "aarch64-unknown-none-softfloat",
        "riscv64gc-unknown-none-elf",
        "loongarch64-unknown-none-softfloat",
    ] {
        let check = ClippyCheck {
            package: "starry-kernel".into(),
            kind: ClippyCheckKind::Configuration {
                name: format!("{target}-system"),
                features: Vec::new(),
                rustflags: Vec::new(),
            },
            target: Some(target.into()),
            env: vec![("AX_TARGET".into(), target.into())],
        };

        let invocation = check.cargo_invocation();

        assert!(invocation.args.windows(2).any(|args| {
            args[0] == "--target"
                && args[1].ends_with(&format!("scripts/targets/bare/{target}.json"))
        }));
        assert!(
            invocation
                .args
                .windows(2)
                .any(|args| args == ["-Z", "json-target-spec"])
        );
        assert!(
            invocation
                .args
                .windows(2)
                .any(|args| args == ["-Z", "build-std=core,alloc"])
        );
        assert!(
            invocation
                .env
                .contains(&("CARGO_UNSTABLE_JSON_TARGET_SPEC".into(), "true".into()))
        );
        assert!(!invocation.args.join("\n").contains("target-feature=-ual"));
    }
}

#[test]
fn clippy_preserves_non_bare_docs_rs_targets() {
    let target = "x86_64-unknown-linux-gnu";
    let check = ClippyCheck {
        package: "host-package".into(),
        kind: ClippyCheckKind::Base,
        target: Some(target.into()),
        env: Vec::new(),
    };

    let invocation = check.cargo_invocation();

    assert!(
        invocation
            .args
            .windows(2)
            .any(|args| args == ["--target", target])
    );
    assert!(!invocation.args.iter().any(|arg| arg == "json-target-spec"));
    assert!(invocation.env.is_empty());
}

#[test]
fn incremental_selection_checks_changed_packages_and_affected_os_roots_only() {
    let selected = incremental_clippy_selections(
        vec!["shared".into()],
        vec![
            "app".into(),
            "ax-std".into(),
            "intermediate".into(),
            "shared".into(),
            "starryos".into(),
        ],
    );

    assert_eq!(
        selected,
        vec!["shared".to_string(), "ax-std".into(), "starryos".into()]
    );
}

#[test]
fn incremental_selection_for_x86_apic_change_omits_unrelated_workspace_packages() {
    let selected = incremental_clippy_selections(
        vec![
            "someboot".into(),
            "somehal".into(),
            "x86-apic-driver".into(),
        ],
        vec![
            "ax-std".into(),
            "someboot".into(),
            "somehal".into(),
            "starryos".into(),
            "unrelated".into(),
            "x86-apic-driver".into(),
        ],
    );

    assert_eq!(
        selected,
        vec![
            "someboot".to_string(),
            "somehal".into(),
            "x86-apic-driver".into(),
            "ax-std".into(),
            "starryos".into(),
        ]
    );
}

#[test]
fn incremental_selection_adds_only_affected_os_root() {
    let selected = incremental_clippy_selections(
        vec!["alpha".into()],
        vec!["alpha".into(), "intermediate".into(), "ax-std".into()],
    );

    assert_eq!(selected, vec!["alpha".to_string(), "ax-std".into()]);
}

#[test]
fn incremental_selection_omits_unaffected_os_roots_and_top_levels() {
    let selected = incremental_clippy_selections(
        vec!["alpha".into()],
        vec![
            "alpha".into(),
            "intermediate".into(),
            "app".into(),
            "axvisor".into(),
        ],
    );

    assert_eq!(selected, vec!["alpha"]);
}

#[test]
fn incremental_selection_deduplicates_changed_os_root() {
    let selected = incremental_clippy_selections(
        vec!["starryos".into(), "starryos".into()],
        vec!["starryos".into()],
    );

    assert_eq!(selected, vec!["starryos"]);
}

#[test]
fn incremental_selection_keeps_changed_unsupported_package_for_filtering() {
    let selected = incremental_clippy_selections(
        vec!["axvisor".into()],
        vec!["axvisor".into(), "axvm".into()],
    );

    assert_eq!(selected, vec!["axvisor"]);
}

#[test]
fn incremental_os_roots_expand_full_clippy_matrix_with_no_deps() {
    let packages = vec![
        pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("feat-a", &[])],
            None,
        ),
        pkg_with_metadata(
            "starryos",
            "starryos 0.1.0 (path+file:///tmp/starryos)",
            &[("feat-b", &[])],
            serde_json::json!({
                "clippy": {
                    "configurations": [{
                        "name": "aarch64-system",
                        "target": "aarch64-unknown-none-softfloat",
                    }],
                },
            }),
        ),
    ];
    let metadata = metadata_with_resolve(
        packages.clone(),
        &[("alpha", &[]), ("starryos", &["alpha"])],
    );
    let selections = incremental_clippy_selections(
        vec!["alpha".into()],
        vec!["alpha".into(), "starryos".into()],
    )
    .into_iter()
    .map(|name| {
        packages
            .iter()
            .find(|package| package.name == name)
            .cloned()
            .unwrap()
    })
    .collect::<Vec<_>>();

    let checks = crate::clippy::expand::expand_clippy_checks(&selections, &metadata).unwrap();

    assert_eq!(
        checks.iter().map(|check| check.label()).collect::<Vec<_>>(),
        vec![
            "alpha (base)",
            "alpha (feature: feat-a)",
            "starryos (base)",
            "starryos (feature: feat-b)",
            "starryos (configuration: aarch64-system, features: , target: \
             aarch64-unknown-none-softfloat)",
        ]
    );
    assert!(
        checks
            .iter()
            .all(|check| check.cargo_args().contains(&"--no-deps".into()))
    );
}

#[test]
fn axstd_default_feature_no_deps_check_keeps_no_deps_flag() {
    let check = ClippyCheck {
        package: AXSTD_STD_PACKAGE.into(),
        kind: ClippyCheckKind::Feature(AXSTD_STD_DEFAULT_FEATURE.into()),
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
        &[("fp-simd", &[])],
        Some(&["loongarch64-unknown-none", "riscv64gc-unknown-none-elf"]),
    )]);

    let has_feature_on_target = |feature: &str, target: &str| {
        checks.iter().any(|check| {
            matches!(&check.kind, ClippyCheckKind::Feature(check_feature) if check_feature == feature)
                && check.target.as_deref() == Some(target)
        })
    };

    assert!(has_feature_on_target(
        "fp-simd",
        "loongarch64-unknown-none-softfloat"
    ));
    assert!(has_feature_on_target(
        "fp-simd",
        "riscv64gc-unknown-none-elf"
    ));
}

#[test]
fn ax_hal_target_only_features_are_skipped_for_host_clippy() {
    let checks = expand(&[pkg(
        "ax-hal",
        "ax-hal 0.1.0 (path+file:///tmp/ax-hal)",
        &[("fp-simd", &[])],
        None,
    )]);

    assert!(checks.iter().any(|check| {
        matches!(&check.kind, ClippyCheckKind::Feature(feature) if feature == "fp-simd")
    }));
}

#[test]
fn ax_hal_platform_feature_forwards_are_filtered_by_target_arch() {
    let checks = expand(&[pkg(
        "platform-forwarder",
        "platform-forwarder 0.1.0 (path+file:///tmp/platform-forwarder)",
        &[("fp-simd", &["ax-hal/fp-simd"])],
        Some(&["loongarch64-unknown-none", "riscv64gc-unknown-none-elf"]),
    )]);

    let has_feature_on_target = |feature: &str, target: &str| {
        checks.iter().any(|check| {
            matches!(&check.kind, ClippyCheckKind::Feature(check_feature) if check_feature == feature)
                && check.target.as_deref() == Some(target)
        })
    };

    assert!(has_feature_on_target(
        "fp-simd",
        "loongarch64-unknown-none-softfloat"
    ));
    assert!(has_feature_on_target(
        "fp-simd",
        "riscv64gc-unknown-none-elf"
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
fn package_clippy_configuration_lints_source_with_rustflags() {
    let checks = expand(&[pkg_with_metadata(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[("axtest", &[]), ("smp", &[])],
        serde_json::json!({
            "clippy": {
                "configurations": [{
                    "name": "loongarch64-axtest-source",
                    "target": "loongarch64-unknown-none-softfloat",
                    "features": ["axtest", "smp"],
                    "rustflags": ["--cfg", "axtest", "--check-cfg", "cfg(axtest)"],
                }],
            },
        }),
    )]);
    let check = checks
        .iter()
        .find(|check| {
            check
                .label()
                .contains("configuration: loongarch64-axtest-source")
        })
        .expect("source configuration should be planned");

    assert_eq!(
        check.cargo_args(),
        [
            "clippy",
            "--no-deps",
            "-p",
            "alpha",
            "--features",
            "axtest,smp",
            "--target",
            "loongarch64-unknown-none-softfloat",
            "--",
            "--cfg",
            "axtest",
            "--check-cfg",
            "cfg(axtest)",
            "-D",
            "warnings",
        ]
    );
}

#[test]
fn selected_package_expands_package_clippy_configurations() {
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
    let checks = crate::clippy::expand::expand_clippy_checks(&[package], &metadata).unwrap();

    assert_eq!(checks[0].label(), "alpha (base)");
    assert_eq!(
        checks[1].label(),
        "alpha (configuration: aarch64-system, features: , target: aarch64-unknown-none-softfloat)"
    );
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
fn package_clippy_configuration_rejects_empty_rustflags() {
    let package = pkg_with_metadata(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[],
        serde_json::json!({
            "clippy": {
                "configurations": [{
                    "name": "aarch64-source",
                    "target": "aarch64-unknown-none-softfloat",
                    "rustflags": ["--cfg", ""],
                }],
            },
        }),
    );

    let err = package_clippy_configurations(&package).unwrap_err();

    assert_eq!(
        err.to_string(),
        "clippy configuration `aarch64-source` rustflag for `alpha` must be non-empty and trimmed"
    );
}

#[test]
fn starry_clippy_configurations_match_qemu_builds() {
    let workspace_root = crate::context::find_workspace_root();
    let manifest: StarryKernelManifest = toml::from_str(
        &std::fs::read_to_string(workspace_root.join("os/StarryOS/kernel/Cargo.toml")).unwrap(),
    )
    .unwrap();

    for (name, arch, relative_build_path) in [
        (
            "riscv64-system",
            "riscv64",
            "test-suit/starryos/qemu/build-riscv64gc-unknown-none-elf.toml",
        ),
        (
            "aarch64-system",
            "aarch64",
            "test-suit/starryos/qemu/build-aarch64-unknown-none-softfloat.toml",
        ),
        (
            "aarch64-system-rga",
            "aarch64",
            "test-suit/starryos/qemu-rga/build-aarch64-unknown-none-softfloat.toml",
        ),
        (
            "loongarch64-system",
            "loongarch64",
            "test-suit/starryos/qemu/build-loongarch64-unknown-none-softfloat.toml",
        ),
        (
            "x86_64-system",
            "x86_64",
            "test-suit/starryos/qemu/build-x86_64-unknown-none.toml",
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
        assert_eq!(configuration.env.get("AX_ARCH"), Some(&arch.into()));
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

#[test]
fn starry_axtest_source_clippy_configurations_match_ktest_builds() {
    let workspace_root = crate::context::find_workspace_root();
    let manifest: StarryKernelManifest = toml::from_str(
        &std::fs::read_to_string(workspace_root.join("os/StarryOS/kernel/Cargo.toml")).unwrap(),
    )
    .unwrap();
    let axtest = manifest
        .tests
        .iter()
        .find(|target| target.name == "axtest_kernel")
        .unwrap();

    for (name, arch, relative_build_path) in [
        (
            "riscv64-axtest-source",
            "riscv64",
            "os/StarryOS/configs/board/qemu-riscv64.toml",
        ),
        (
            "aarch64-axtest-source",
            "aarch64",
            "os/StarryOS/configs/board/qemu-aarch64.toml",
        ),
        (
            "loongarch64-axtest-source",
            "loongarch64",
            "os/StarryOS/configs/board/qemu-loongarch64.toml",
        ),
        (
            "x86_64-axtest-source",
            "x86_64",
            "os/StarryOS/configs/board/qemu-x86_64.toml",
        ),
    ] {
        let build: StarryAxtestBuildConfiguration = toml::from_str(
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
        expected_features.extend(axtest.required_features.iter().cloned());
        expected_features.sort();
        expected_features.dedup();

        assert_eq!(configuration.features, expected_features);
        assert_eq!(configuration.target, build.target);
        assert_eq!(configuration.rustflags, crate::ktest::AXTEST_RUSTFLAGS);
        assert_eq!(configuration.env.get("AX_ARCH"), Some(&arch.into()));
        assert_eq!(
            configuration.env.get("AX_TARGET"),
            Some(&configuration.target)
        );
        assert_eq!(
            configuration.env.get("AX_LOG"),
            Some(&build.log.to_ascii_lowercase())
        );
    }
}

#[derive(serde::Deserialize)]
struct StarryKernelManifest {
    package: StarryKernelPackage,
    #[serde(rename = "test")]
    tests: Vec<StarryTestTarget>,
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
    #[serde(default)]
    rustflags: Vec<String>,
    env: std::collections::BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
struct StarryTestTarget {
    name: String,
    #[serde(rename = "required-features")]
    required_features: Vec<String>,
}

#[derive(serde::Deserialize)]
struct StarryBuildConfiguration {
    features: Vec<String>,
    max_cpu_num: usize,
    log: String,
    target: String,
}

#[derive(serde::Deserialize)]
struct StarryAxtestBuildConfiguration {
    features: Vec<String>,
    log: String,
    target: String,
}
