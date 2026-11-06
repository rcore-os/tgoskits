use super::{
    plan::{
        AARCH64_TARGET, DiscoveredKtestPackage, KtestExecutionUnit, KtestRuntime,
        LOONGARCH64_TARGET, QemuPlanSelector, RISCV64_TARGET, X86_64_TARGET, build_qemu_plan,
        run_plan_units,
    },
    *,
};
use crate::support::qemu_success::{QemuSuccessOutput, verify_qemu_success_contract};

fn discovered_package(
    name: &str,
    uses_workspace_axtest: bool,
    runtime: KtestRuntime,
    targets: &[(&str, bool)],
    test_targets: Option<&[&str]>,
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
                board_only: false,
            })
            .collect(),
        test_targets: test_targets
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
fn test_targets_without_a_supported_bare_metal_target_are_rejected() {
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
                board_only: false,
            },
            KtestTarget {
                name: "kernel".into(),
                kind: KtestTargetKind::Test,
                harness: false,
                required_features: Vec::new(),
                board_only: false,
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
                board_only: false,
            },
            KtestTarget {
                name: "second".into(),
                kind: KtestTargetKind::Test,
                harness: false,
                required_features: Vec::new(),
                board_only: false,
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
            board_only: false,
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
        board_only: false,
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
        board_only: false,
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

#[test]
fn declared_ktest_targets_win_over_the_docs_rs_fallback() {
    let metadata = serde_json::json!({
        "axtest": { "runtime": "axvisor", "target_triples": [AARCH64_TARGET], "targets": { "remote": { "board_only": true } } },
        "docs": { "rs": { "targets": ["x86_64-unknown-linux-gnu"] } },
    });

    let axtest = axtest_metadata("declared", &metadata).unwrap();

    assert_eq!(axtest.runtime, Some(KtestRuntime::Axvisor));
    assert_eq!(axtest.board_only.get("remote"), Some(&true));
    assert_eq!(
        axtest.test_targets.as_deref(),
        Some(&[AARCH64_TARGET.to_string()][..])
    );

    // Without a declared list the host-doc targets stay the fallback, and the
    // per-target flag table does not declare a target list of its own.
    let metadata = serde_json::json!({
        "axtest": { "targets": { "remote": { "board_only": true } } },
        "docs": { "rs": { "targets": [AARCH64_TARGET] } },
    });

    let axtest = axtest_metadata("fallback", &metadata).unwrap();

    assert_eq!(axtest.runtime, None);
    assert_eq!(
        axtest.test_targets.as_deref(),
        Some(&[AARCH64_TARGET.to_string()][..])
    );
    assert_eq!(axtest.board_only.get("remote"), Some(&true));
    assert_eq!(axtest.board_only.get("local"), None);

    // A package that declares neither list runs on the default architecture.
    assert_eq!(
        axtest_metadata("generic", &serde_json::json!({})).unwrap(),
        AxtestMetadata::default()
    );
}

#[test]
fn malformed_axtest_metadata_is_rejected_instead_of_guessed() {
    for metadata in [
        serde_json::json!({ "axtest": { "targets": AARCH64_TARGET } }),
        serde_json::json!({ "axtest": { "target_triples": [1] } }),
        serde_json::json!({ "axtest": { "targets": { "remote": { "board_only": "yes" } } } }),
        serde_json::json!({ "axtest": { "runtime": "kernel" } }),
        serde_json::json!({ "docs": { "rs": { "targets": "x86_64-unknown-none" } } }),
    ] {
        let error = axtest_metadata("broken", &metadata).unwrap_err();

        assert!(
            error.to_string().contains("broken"),
            "unexpected error for {metadata}: {error}"
        );
    }
}

/// Synthetic metadata for one package with a QEMU target and a board-only
/// target; the names are placeholders for the discovery rules under test.
fn mixed_runtime_package() -> DiscoveredKtestPackage {
    let mut package = discovered_package(
        "mixed",
        true,
        KtestRuntime::Axvisor,
        &[("local", false), ("remote", false)],
        Some(&[X86_64_TARGET]),
    );
    package.targets[1].board_only = true;
    package
}

/// Asserts that one completion condition accepts a finished suite and rejects
/// an unfinished or empty one.
fn assert_suite_gate(patterns: &[String]) {
    assert_eq!(
        patterns.len(),
        1,
        "the suite gate must be the only success condition"
    );
    let gate = regex::Regex::new(&patterns[0]).unwrap();

    assert!(gate.is_match(
        "AXTEST_CASE status=pass module=demo name=case\nAXTEST_SUMMARY pass=2 fail=0 skip=0 \
         total=2\nAXTEST_SUITE_OK\n"
    ));
    // Use the runner's streaming DFA as well as the test regex engine. Some
    // regex features accepted by `regex` cannot compile into that DFA.
    let output = QemuSuccessOutput::new(patterns);
    output.append(b"AXTEST_CASE status=pass module=demo name=case\n");
    output.append(b"AXTEST_SUMMARY pass=1 fail=0 skip=0 total=1\n");
    output.append(b"AXTEST_SUITE_OK\n");
    verify_qemu_success_contract(Ok(()), Some(&output)).unwrap();
    assert!(
        !gate.is_match("GUEST_PASS\n"),
        "a guest success is not a suite result"
    );
    assert!(
        !gate.is_match("AXTEST_SUMMARY pass=0 fail=0 skip=1 total=1\nAXTEST_SUITE_OK\n"),
        "a suite without a passing case is not a result"
    );
    assert!(
        !gate.is_match("AXTEST_SUMMARY pass=1 fail=0 skip=0 total=1\nAXTEST_SUITE_OK\n"),
        "a summary cannot replace an executed passing case"
    );
    assert!(
        !gate.is_match("AXTEST_SUITE_OK\nAXTEST_SUMMARY pass=1 fail=0 skip=0 total=1\n"),
        "the summary must be verified before the final marker"
    );
}

#[test]
fn board_only_targets_are_never_implicit_qemu_work() {
    let plan = build_qemu_plan(&[mixed_runtime_package()], &QemuPlanSelector::default()).unwrap();

    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].test, "local");

    let mut board_package = discovered_package(
        "remote-only",
        true,
        KtestRuntime::Axvisor,
        &[("remote", false)],
        Some(&[X86_64_TARGET]),
    );
    board_package.targets[0].board_only = true;

    let plan = build_qemu_plan(&[board_package], &QemuPlanSelector::default()).unwrap();

    assert!(
        plan.is_empty(),
        "a package that only owns board-only targets is not a manifest error"
    );
}

#[test]
fn explicit_test_selection_rejects_board_only_targets_and_keeps_others() {
    let packages = [mixed_runtime_package()];
    let selector = QemuPlanSelector {
        packages: vec!["mixed".into()],
        tests: vec!["remote".into()],
        ..QemuPlanSelector::default()
    };

    let error = build_qemu_plan(&packages, &selector).unwrap_err();

    assert!(error.to_string().contains("board-only"));
    assert!(error.to_string().contains("ktest board"));
    assert!(error.to_string().contains("remote"));

    let selector = QemuPlanSelector {
        tests: vec!["local".into()],
        ..QemuPlanSelector::default()
    };
    let plan = build_qemu_plan(&packages, &selector).unwrap();

    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].test, "local");
}

#[test]
fn qemu_markers_make_the_suite_gate_the_only_success_condition() {
    let mut qemu = QemuConfig {
        shell_check_steps: vec![ShellCheckStep {
            success_regex: Some(vec!["GUEST_PASS".into(), AXTEST_SUITE_OK.into()]),
            ..ShellCheckStep::default()
        }],
        ..QemuConfig::default()
    };

    apply_axtest_qemu_markers(&mut qemu);

    assert_eq!(qemu.shell_check_steps.len(), 1);
    let patterns = qemu.shell_check_steps[0].success_regex.clone().unwrap();
    assert_suite_gate(&patterns);
    for pattern in [PANIC_FAIL, AXTEST_SUITE_FAIL, AXTEST_CASE_FAIL] {
        assert!(qemu.fail_regex.iter().any(|candidate| candidate == pattern));
    }
}

#[test]
fn board_markers_keep_boot_steps_and_add_a_mandatory_suite_gate() {
    let mut board = BoardRunConfig {
        shell_check_steps: vec![ShellCheckStep {
            shell_prefix: Some("root@board:~#".into()),
            shell_cmd: Some("run-guest".into()),
            success_regex: Some(vec!["GUEST_PASS".into()]),
            fail_regex: Some(vec!["GUEST_FAIL".into()]),
            ..ShellCheckStep::default()
        }],
        fail_regex: vec!["custom".into()],
        ..BoardRunConfig::default()
    };

    apply_axtest_board_markers(&mut board);

    assert_eq!(board.shell_check_steps.len(), 2);
    let boot_step = &board.shell_check_steps[0];
    assert_eq!(boot_step.shell_cmd.as_deref(), Some("run-guest"));
    assert_eq!(
        boot_step.success_regex.as_deref(),
        Some(&["GUEST_PASS".to_string()][..])
    );
    let gate_step = &board.shell_check_steps[1];
    assert!(gate_step.shell_cmd.is_none());
    let patterns = gate_step.success_regex.clone().unwrap();
    assert_suite_gate(&patterns);
    for pattern in [PANIC_FAIL, AXTEST_SUITE_FAIL, AXTEST_CASE_FAIL, "custom"] {
        assert!(
            board
                .fail_regex
                .iter()
                .any(|candidate| candidate == pattern)
        );
    }
    assert_eq!(
        boot_step.fail_regex.as_deref(),
        Some(&["GUEST_FAIL".to_string()][..])
    );
}

#[tokio::test]
async fn plan_failures_aggregate_unit_and_resource_preparation_errors() {
    let units = ["first", "middle", "last"]
        .into_iter()
        .map(|test| KtestExecutionUnit {
            package: "mixed".into(),
            test: test.into(),
            runtime: KtestRuntime::Axvisor,
            arch: "x86_64".into(),
            target: X86_64_TARGET.into(),
        })
        .collect::<Vec<_>>();

    let failures = run_plan_units(units, true, |unit| async move {
        anyhow::ensure!(
            unit.test != "middle",
            "host_prepare `prepare.py` failed with status exit status: 3"
        );
        Ok(())
    })
    .await;

    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].unit.test, "middle");
    assert!(
        failures[0].error.to_string().contains("host_prepare"),
        "the outermost summary must carry the preparation failure"
    );
    assert!(finish_qemu_plan(Vec::new()).is_ok());
    let error = finish_qemu_plan(failures).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("1 axtest QEMU execution unit(s) failed")
    );
}
