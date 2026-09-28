use super::{
    plan::{
        AARCH64_TARGET, DiscoveredKtestPackage, KtestExecutionUnit, KtestRuntime,
        LOONGARCH64_TARGET, QemuPlanSelector, RISCV64_TARGET, X86_64_TARGET, build_qemu_plan,
        run_plan_units,
    },
    *,
};

fn discovered_package(
    name: &str,
    uses_workspace_axtest: bool,
    runtime: KtestRuntime,
    targets: &[(&str, bool)],
    docs_rs_targets: Option<&[&str]>,
) -> DiscoveredKtestPackage {
    DiscoveredKtestPackage {
        name: name.into(),
        manifest_path: PathBuf::from(format!("/repo/{name}/Cargo.toml")),
        uses_workspace_axtest,
        runtime,
        targets: targets
            .iter()
            .map(|(target, harness)| KtestTarget {
                name: (*target).into(),
                kind: KtestTargetKind::Test,
                harness: *harness,
                required_features: Vec::new(),
            })
            .collect(),
        docs_rs_targets: docs_rs_targets
            .map(|targets| targets.iter().map(|target| (*target).to_string()).collect()),
    }
}

#[test]
fn workspace_plan_skips_packages_without_direct_axtest_dependency() {
    let packages = [
        discovered_package(
            "plain",
            false,
            KtestRuntime::Arceos,
            &[("axtest", false)],
            None,
        ),
        discovered_package(
            "tested",
            true,
            KtestRuntime::Arceos,
            &[("axtest", false)],
            None,
        ),
    ];

    let plan = build_qemu_plan(&packages, &QemuPlanSelector::default()).unwrap();

    assert_eq!(plan[0].package, "tested");
}

#[test]
fn explicit_package_without_axtest_dependency_is_an_error() {
    let packages = [discovered_package(
        "plain",
        false,
        KtestRuntime::Arceos,
        &[("axtest", false)],
        None,
    )];
    let selector = QemuPlanSelector {
        packages: vec!["plain".into()],
        ..QemuPlanSelector::default()
    };

    let error = build_qemu_plan(&packages, &selector).unwrap_err();

    assert!(error.to_string().contains("as a dependency"));
    assert!(error.to_string().contains("plain"));
}

#[test]
fn axtest_package_without_harness_false_target_is_a_manifest_error() {
    let packages = [discovered_package(
        "broken",
        true,
        KtestRuntime::Arceos,
        &[("axtest", true)],
        None,
    )];

    let error = build_qemu_plan(&packages, &QemuPlanSelector::default()).unwrap_err();

    assert!(error.to_string().contains("harness=false"));
    assert!(error.to_string().contains("broken"));
}

#[test]
fn workspace_plan_expands_multiple_test_bins_and_docs_rs_arches_in_stable_order() {
    let packages = [
        discovered_package(
            "zeta",
            true,
            KtestRuntime::Arceos,
            &[("second", false), ("first", false)],
            Some(&[RISCV64_TARGET, X86_64_TARGET]),
        ),
        discovered_package(
            "alpha",
            true,
            KtestRuntime::Starry,
            &[("kernel", false)],
            Some(&[LOONGARCH64_TARGET, AARCH64_TARGET]),
        ),
    ];

    let plan = build_qemu_plan(&packages, &QemuPlanSelector::default()).unwrap();
    let keys = plan
        .iter()
        .map(|unit| {
            (
                unit.package.as_str(),
                unit.test.as_str(),
                unit.arch.as_str(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        keys,
        [
            ("alpha", "kernel", "aarch64"),
            ("alpha", "kernel", "loongarch64"),
            ("zeta", "first", "riscv64"),
            ("zeta", "first", "x86_64"),
            ("zeta", "second", "riscv64"),
            ("zeta", "second", "x86_64"),
        ]
    );
}

#[test]
fn docs_rs_targets_without_supported_bare_metal_target_are_rejected() {
    let packages = [discovered_package(
        "host-only",
        true,
        KtestRuntime::Arceos,
        &[("axtest", false)],
        Some(&["x86_64-unknown-linux-gnu"]),
    )];

    let error = build_qemu_plan(&packages, &QemuPlanSelector::default()).unwrap_err();

    assert!(error.to_string().contains("bare-metal"));
    assert!(error.to_string().contains("host-only"));
}

#[test]
fn board_runtime_is_excluded_from_workspace_qemu_plan() {
    let packages = [discovered_package(
        "board-test",
        true,
        KtestRuntime::Board,
        &[("axtest", false)],
        Some(&[RISCV64_TARGET]),
    )];

    let plan = build_qemu_plan(&packages, &QemuPlanSelector::default()).unwrap();

    assert!(plan.is_empty());
}

#[test]
fn config_overrides_require_one_execution_unit() {
    let args = ArgsKtestQemu {
        config: Some(PathBuf::from("build.toml")),
        ..ArgsKtestQemu::default()
    };
    let units = ["first", "second"]
        .into_iter()
        .map(|test| KtestExecutionUnit {
            package: "demo".into(),
            test: test.into(),
            runtime: KtestRuntime::Arceos,
            arch: "x86_64".into(),
            target: X86_64_TARGET.into(),
        })
        .collect::<Vec<_>>();

    let error = validate_unique_config_overrides(&args, &units).unwrap_err();

    assert!(error.to_string().contains("exactly one"));
}

#[tokio::test]
async fn plan_runner_invokes_each_unit_once_and_honors_fail_fast_policy() {
    let units = ["first", "second", "third"]
        .into_iter()
        .map(|test| KtestExecutionUnit {
            package: "demo".into(),
            test: test.into(),
            runtime: KtestRuntime::Arceos,
            arch: "x86_64".into(),
            target: X86_64_TARGET.into(),
        })
        .collect::<Vec<_>>();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls_for_runner = calls.clone();

    let failures = run_plan_units(units, false, move |unit| {
        let calls = calls_for_runner.clone();
        async move {
            calls.lock().unwrap().push(unit.test.clone());
            anyhow::ensure!(unit.test != "second", "expected failure");
            Ok(())
        }
    })
    .await;

    assert_eq!(*calls.lock().unwrap(), ["first", "second"]);
    assert_eq!(failures[0].unit.test, "second");

    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls_for_runner = calls.clone();
    let failures = run_plan_units(
        ["first", "second", "third"]
            .into_iter()
            .map(|test| KtestExecutionUnit {
                package: "demo".into(),
                test: test.into(),
                runtime: KtestRuntime::Arceos,
                arch: "x86_64".into(),
                target: X86_64_TARGET.into(),
            })
            .collect(),
        true,
        move |unit| {
            let calls = calls_for_runner.clone();
            async move {
                calls.lock().unwrap().push(unit.test.clone());
                anyhow::ensure!(unit.test != "second", "expected failure");
                Ok(())
            }
        },
    )
    .await;

    assert_eq!(*calls.lock().unwrap(), ["first", "second", "third"]);
    assert!(failures.iter().any(|failure| failure.unit.test == "second"));
}

#[test]
fn selects_only_harness_false_test_target() {
    let package = KtestPackage {
        name: "demo".into(),
        targets: vec![
            KtestTarget {
                name: "unit".into(),
                kind: KtestTargetKind::Lib,
                harness: true,
                required_features: Vec::new(),
            },
            KtestTarget {
                name: "kernel".into(),
                kind: KtestTargetKind::Test,
                harness: false,
                required_features: Vec::new(),
            },
        ],
    };

    let selected = select_ktest_target(&package, None).unwrap();

    assert_eq!(selected.name, "kernel");
}

#[test]
fn rejects_ambiguous_harness_false_test_targets_without_explicit_name() {
    let package = KtestPackage {
        name: "demo".into(),
        targets: vec![
            KtestTarget {
                name: "first".into(),
                kind: KtestTargetKind::Test,
                harness: false,
                required_features: Vec::new(),
            },
            KtestTarget {
                name: "second".into(),
                kind: KtestTargetKind::Test,
                harness: false,
                required_features: Vec::new(),
            },
        ],
    };

    let err = select_ktest_target(&package, None).unwrap_err();

    assert!(err.to_string().contains("multiple harness=false"));
    assert!(err.to_string().contains("first"));
    assert!(err.to_string().contains("second"));
}

#[test]
fn explicit_target_must_be_harness_false_test() {
    let package = KtestPackage {
        name: "demo".into(),
        targets: vec![KtestTarget {
            name: "unit".into(),
            kind: KtestTargetKind::Test,
            harness: true,
            required_features: Vec::new(),
        }],
    };

    let err = select_ktest_target(&package, Some("unit")).unwrap_err();

    assert!(err.to_string().contains("harness=false"));
}

#[test]
fn prepare_ktest_cargo_replaces_bin_selector_with_test_target() {
    let mut cargo = Cargo {
        package: "demo".into(),
        bin: Some("old-bin".into()),
        args: vec![
            "--bin".into(),
            "old-bin".into(),
            "--test=old-test".into(),
            "--release".into(),
        ],
        features: vec![],
        ..Cargo::default()
    };
    let target = KtestTarget {
        name: "kernel".into(),
        kind: KtestTargetKind::Test,
        harness: false,
        required_features: vec!["extra".into()],
    };

    prepare_ktest_cargo(&mut cargo, &target, KtestRuntime::Arceos, false);

    assert!(cargo.bin.is_none());
    assert_eq!(cargo.test.as_deref(), Some("kernel"));
    assert!(cargo.args.iter().any(|arg| arg == "--release"));
    assert!(
        !cargo
            .args
            .iter()
            .any(|arg| arg == "--bin" || arg == "--test=old-test")
    );
    assert!(cargo.features.iter().any(|feature| feature == "axtest"));
    assert!(cargo.features.iter().any(|feature| feature == "extra"));
    assert!(
        cargo
            .features
            .iter()
            .any(|feature| feature == "ax-std/arceos")
    );
    assert!(
        cargo
            .env
            .get("CARGO_ENCODED_RUSTFLAGS")
            .is_some_and(|flags| flags.contains("cfg(axtest)"))
    );
}

#[test]
fn prepare_ktest_cargo_preserves_inline_target_rustflags() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        package: "demo".into(),
        args: vec![
            "--config".into(),
            concat!(
                "target.x86_64-unknown-none.rustflags=[",
                "\"-Crelocation-model=pic\", ",
                "\"-Clink-args=-Tlinker.x\"",
                "]"
            )
            .into(),
        ],
        ..Cargo::default()
    };
    let target = KtestTarget {
        name: "kernel".into(),
        kind: KtestTargetKind::Test,
        harness: false,
        required_features: Vec::new(),
    };

    prepare_ktest_cargo(&mut cargo, &target, KtestRuntime::Arceos, true);

    let args = cargo.args.join("\n");
    assert!(args.contains("-Clink-args=-Tlinker.x"));
    assert!(args.contains("cfg(axtest)"));
    assert!(args.contains("-Cinstrument-coverage"));
    assert!(
        !cargo.env.contains_key("CARGO_ENCODED_RUSTFLAGS"),
        "encoded rustflags would shadow the inline target linker contract"
    );
}

#[test]
fn coverage_paths_are_isolated_by_package_test_and_target() {
    let root = tempfile::tempdir().unwrap();
    let first = crate::support::axtest_coverage::AxtestCoveragePaths::new(
        root.path(),
        "demo",
        "first",
        X86_64_TARGET,
    )
    .unwrap();
    let second = crate::support::axtest_coverage::AxtestCoveragePaths::new(
        root.path(),
        "demo",
        "second",
        X86_64_TARGET,
    )
    .unwrap();
    let other_arch = crate::support::axtest_coverage::AxtestCoveragePaths::new(
        root.path(),
        "demo",
        "first",
        RISCV64_TARGET,
    )
    .unwrap();

    assert_ne!(first.profraw_path, second.profraw_path);
    assert_ne!(first.profraw_path, other_arch.profraw_path);
    assert!(
        first
            .profraw_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("demo-first-x86_64-unknown-none")
    );
}
