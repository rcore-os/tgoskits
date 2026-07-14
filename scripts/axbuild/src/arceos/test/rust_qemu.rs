use std::{fs, sync::Arc};

use anyhow::{Context, bail};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};
use regex::Regex;

use super::{
    ARCEOS_RUST_ALL_FEATURE, ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE,
    ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE, ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE,
    ARCEOS_RUST_LOCKDEP_DETECT_FEATURE, ARCEOS_RUST_QEMU_FEATURES,
    ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE,
    assets::test_build_args,
    discovery::discover_rust_qemu_cases,
    runner::run_prepared_qemu_groups,
    types::{ArceosRustQemuCase, PreparedArceosRustQemuCase},
};
use crate::{
    arceos::{ArceOS, build, ensure_qemu_runtime_assets},
    context::SnapshotPersistence,
    test::{host_http::HostHttpServerGuard, qemu as qemu_test},
};

pub(super) async fn test_rust_qemu(
    arceos: &mut ArceOS,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
    symbolize_after: bool,
    keep_qemu_log: bool,
) -> anyhow::Result<()> {
    let cases = discover_rust_qemu_cases(
        arceos,
        arch,
        target,
        selected_case,
        allow_missing_selected_case,
    )?;
    if cases.is_empty() {
        println!(
            "skipping arceos rust qemu tests for arch: {arch} (target: {target}, no matching \
             feature)"
        );
        return Ok(());
    }
    println!(
        "running arceos rust qemu tests for arch: {} (target: {}, cases: {})",
        arch,
        target,
        cases.len()
    );

    let prepared = prepare_rust_qemu_cases(arceos, target, cases).await?;
    run_prepared_qemu_groups(
        arceos,
        "rust",
        "arceos rust",
        &prepared,
        symbolize_after,
        keep_qemu_log,
    )
    .await
}

pub(super) async fn prepare_rust_qemu_cases(
    arceos: &mut ArceOS,
    target: &str,
    cases: Vec<ArceosRustQemuCase>,
) -> anyhow::Result<Vec<PreparedArceosRustQemuCase>> {
    let mut prepared = Vec::with_capacity(cases.len());
    for case in cases {
        let request = arceos.prepare_request(
            test_build_args(&case.package, target, &case.build_config_path),
            Some(case.case.qemu_config_path.clone()),
            None,
            SnapshotPersistence::Discard,
        )?;
        let mut cargo = build::load_cargo_config(&request)?;
        if let Some(feature) = case.feature.as_deref() {
            add_cargo_feature(&mut cargo, feature);
        }
        let mut qemu = arceos
            .load_qemu_config(&request, &cargo)
            .await?
            .with_context(|| {
                format!(
                    "failed to load ArceOS qemu config for case `{}`",
                    case.case.display_name
                )
            })?;
        let build_info: build::ArceosBuildInfo =
            crate::build::load_build_info(&request.build_info_path)?;
        qemu_test::apply_smp_qemu_arg(
            &mut qemu,
            request.smp.or(build_info.max_cpu_num).or(Some(1)),
        );
        apply_rust_qemu_feature_overrides(&mut cargo, &mut qemu, case.feature.as_deref());
        qemu_test::apply_timeout_scale(&mut qemu);
        ensure_qemu_runtime_assets(arceos.app.workspace_root(), &qemu)?;
        prepared.push(PreparedArceosRustQemuCase {
            host_symbolize_success_regex: rust_qemu_host_symbolize_success_regex(
                case.feature.as_deref(),
            ),
            case,
            request,
            cargo,
            qemu,
        });
    }
    Ok(prepared)
}

fn rust_qemu_host_symbolize_success_regex(feature: Option<&str>) -> Vec<String> {
    match feature {
        Some(ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE) => vec![
            r"(?s)BACKTRACE_BLOCK\s+\d+\s+kind=arceos-test-suit-raw-normal\b.*\bdebug::backtrace::nested_c\b.*\bdebug::backtrace::nested_b\b.*\bdebug::backtrace::nested_a\b"
                .to_string(),
            r"(?s)BACKTRACE_BLOCK\s+\d+\s+kind=arceos-test-suit-raw-badfp\b.*BT\s+0\s+ip=0x[0-9a-fA-F]+"
                .to_string(),
        ],
        _ => Vec::new(),
    }
}

fn apply_rust_qemu_feature_overrides(
    cargo: &mut Cargo,
    qemu: &mut QemuConfig,
    feature: Option<&str>,
) {
    match feature {
        Some(ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE) => {
            qemu.success_regex = vec![r"BACKTRACE_BEGIN\b.*\bkind=panic\b".to_string()];
            qemu.fail_regex = vec!["ARCEOS_TEST_FAIL".to_string()];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        Some(ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE) => {
            qemu.success_regex = vec!["Page fault test OK!".to_string()];
            qemu.fail_regex = vec![
                r"(?i)\bpanic(?:ked)?\b".to_string(),
                "page fault handler did not stop the system".to_string(),
            ];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        Some(feature) if is_lockdep_detect_feature(feature) => {
            qemu.success_regex = vec!["lockdep: lock order inversion detected".to_string()];
            qemu.fail_regex =
                vec![r"lockdep did not report an expected .*lock order inversion".to_string()];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        Some(ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE) => {
            qemu.success_regex =
                vec!["task stack guard page hit for .*stack-guard-page-overflow".to_string()];
            qemu.fail_regex = vec!["stack guard page was not hit".to_string()];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        Some("task-wait-queue-remote-wake")
            if cargo.target == "riscv64gc-unknown-none-elf"
                && !qemu.args.iter().any(|arg| arg == "-accel") =>
        {
            qemu.args.push("-accel".to_string());
            qemu.args.push("tcg,thread=single".to_string());
        }
        _ => {}
    }
}

fn is_lockdep_detect_feature(feature: &str) -> bool {
    matches!(feature, ARCEOS_RUST_LOCKDEP_DETECT_FEATURE)
}

fn add_cargo_feature(cargo: &mut Cargo, feature: &str) {
    if !cargo.features.iter().any(|existing| existing == feature) {
        cargo.features.push(feature.to_string());
        cargo.features.sort();
    }
}

pub(super) async fn run_rust_qemu_case(
    arceos: &mut ArceOS,
    case: &PreparedArceosRustQemuCase,
    symbolize_after: bool,
    keep_qemu_log: bool,
) -> anyhow::Result<()> {
    let workspace = arceos.app.workspace_root().to_path_buf();
    let case_name = &case.case.case.name;
    let target = &case.request.target;
    let package = &case.case.package;
    let debug = case.request.debug;

    let auto_symbolize = symbolize_after
        && crate::build::build_info_enables_backtrace_path(&case.case.build_config_path);
    if !case.host_symbolize_success_regex.is_empty() && !auto_symbolize {
        bail!(
            "ArceOS rust qemu case `{case_name}` requires host symbolize assertions; do not use \
             --no-symbolize and keep BACKTRACE/DWARF enabled in the build config"
        );
    }

    let elf = crate::backtrace::std_test_elf_path(&workspace, target, package, debug);
    let stream_session = if auto_symbolize {
        crate::backtrace::BacktraceSymbolizeSession::try_new(&elf, case_name)
    } else {
        None
    };

    let capture_backtrace = if auto_symbolize {
        let dir = crate::context::axbuild_tmp_dir(&workspace).join("qemu-logs");
        fs::create_dir_all(&dir)?;
        Some(crate::backtrace::BacktraceQemuCapture {
            log_path: dir.join(format!("{case_name}-{target}.log")),
            stream_symbolize: stream_session.clone(),
            suppress_terminal_raw_blocks: true,
            write_log_during_capture: keep_qemu_log,
            captured_blocks: Arc::new(std::sync::Mutex::new(Vec::new())),
        })
    } else {
        None
    };

    let log_path = capture_backtrace
        .as_ref()
        .map(|capture| capture.log_path.clone());
    let memory_blocks = capture_backtrace
        .as_ref()
        .map(|capture| capture.captured_blocks.clone());

    let _host_http_server = case
        .case
        .case
        .host_http_server
        .as_ref()
        .map(|config| HostHttpServerGuard::start(config, case_name))
        .transpose()?;

    arceos
        .app
        .run_qemu_with_axtest_coverage(&case.cargo, case.qemu.clone(), capture_backtrace)
        .await
        .with_context(|| format!("failed to run ArceOS rust qemu test case `{case_name}`"))?;

    if auto_symbolize && let Some(path) = log_path {
        let blocks_snapshot = memory_blocks.and_then(|arc| arc.lock().ok().map(|b| b.clone()));
        let symbolized_output = if !case.host_symbolize_success_regex.is_empty() {
            match blocks_snapshot.as_deref() {
                Some(blocks) => {
                    crate::backtrace::symbolize_captured_blocks_to_string(&elf, case_name, blocks)?
                }
                None => None,
            }
        } else {
            None
        };
        let blocks_ref = blocks_snapshot.as_deref();
        let outcome = crate::backtrace::maybe_symbolize_after_qemu(
            &elf,
            &path,
            case_name,
            keep_qemu_log,
            stream_session.as_deref(),
            blocks_ref,
        )?;
        if !case.host_symbolize_success_regex.is_empty() {
            ensure_arceos_host_symbolize_output_matches(
                case_name,
                outcome,
                symbolized_output.as_deref(),
                &case.host_symbolize_success_regex,
            )?;
        }
    }

    Ok(())
}

fn ensure_arceos_host_symbolize_output_matches(
    case_name: &str,
    outcome: crate::backtrace::SymbolizeAfterQemuOutcome,
    output: Option<&str>,
    regexes: &[String],
) -> anyhow::Result<()> {
    if outcome != crate::backtrace::SymbolizeAfterQemuOutcome::Symbolized {
        bail!("host backtrace symbolize did not run for ArceOS rust qemu case `{case_name}`");
    }
    let output =
        output.ok_or_else(|| anyhow::anyhow!("host backtrace symbolize produced no output"))?;
    for pattern in regexes {
        let regex = Regex::new(pattern)
            .with_context(|| format!("invalid host_symbolize_success_regex `{pattern}`"))?;
        if !regex.is_match(output) {
            bail!(
                "host backtrace symbolize output for ArceOS rust qemu case `{case_name}` did not \
                 match `{pattern}`"
            );
        }
    }
    Ok(())
}

pub(super) fn rust_qemu_features_for_run(
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Vec<&'static str>> {
    match selected_case {
        Some(_) => rust_qemu_features_for_list(selected_case, allow_missing_selected_case),
        None => Ok(vec![ARCEOS_RUST_ALL_FEATURE]),
    }
}

pub(super) fn rust_qemu_features_for_list(
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Vec<&'static str>> {
    let Some(selected_case) = selected_case else {
        return Ok(ARCEOS_RUST_QEMU_FEATURES.to_vec());
    };

    let features = ARCEOS_RUST_QEMU_FEATURES
        .iter()
        .copied()
        .filter(|feature| *feature == selected_case)
        .collect::<Vec<_>>();
    if features.is_empty() {
        if allow_missing_selected_case {
            return Ok(Vec::new());
        }
        bail!("unknown ArceOS rust qemu test feature `{selected_case}`");
    }
    Ok(features)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::arceos::test::ARCEOS_RUST_TEST_PACKAGE;

    #[test]
    fn arceos_rust_default_run_selects_all_feature_only() {
        let features = rust_qemu_features_for_run(None, false).unwrap();
        assert_eq!(features, vec![ARCEOS_RUST_ALL_FEATURE]);
    }

    #[test]
    fn arceos_rust_selected_case_is_feature_name() {
        let features = rust_qemu_features_for_list(Some("task-yield"), false).unwrap();
        assert_eq!(features, vec!["task-yield"]);
    }

    #[test]
    fn arceos_rust_selected_cases_include_restored_coverage_features() {
        for feature in [
            ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE,
            ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE,
            ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE,
            "fs-basic",
            "lockdep-baseline",
            ARCEOS_RUST_LOCKDEP_DETECT_FEATURE,
            "net-loopback",
            ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE,
        ] {
            let features = rust_qemu_features_for_list(Some(feature), false).unwrap();
            assert_eq!(features, vec![feature]);
        }
    }

    #[test]
    fn arceos_rust_debug_backtrace_requires_symbolized_frames() {
        let regexes =
            rust_qemu_host_symbolize_success_regex(Some(ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE));
        assert_eq!(regexes.len(), 2);

        let output = r#"
BACKTRACE_BLOCK 0 kind=arceos-test-suit-raw-normal arch=x86_64
BT 0 ip=0x10 fp=0x20 arceos_test_suit::debug::backtrace::nested_c
BT 1 ip=0x11 fp=0x21 arceos_test_suit::debug::backtrace::nested_b
BT 2 ip=0x12 fp=0x22 arceos_test_suit::debug::backtrace::nested_a
BACKTRACE_BLOCK 1 kind=arceos-test-suit-raw-badfp arch=x86_64
BT 0 ip=0x1 fp=0x2
"#;
        for pattern in &regexes {
            assert!(Regex::new(pattern).unwrap().is_match(output));
        }
    }

    #[test]
    fn arceos_rust_page_fault_qemu_uses_page_fault_result_regex() {
        let mut cargo = rust_test_cargo_for_target("x86_64-unknown-none");
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(
            &mut cargo,
            &mut qemu,
            Some(ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE),
        );

        assert_eq!(qemu.success_regex, vec!["Page fault test OK!"]);
        assert_eq!(
            qemu.fail_regex,
            vec![
                r"(?i)\bpanic(?:ked)?\b",
                "page fault handler did not stop the system"
            ]
        );
        assert_eq!(qemu.timeout, Some(30));
    }

    #[test]
    fn arceos_rust_stack_guard_page_qemu_uses_guard_page_result_regex() {
        let mut cargo = rust_test_cargo_for_target("x86_64-unknown-none");
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![
                r"(?i)\bpanic(?:ked)?\b".to_string(),
                "ARCEOS_TEST_FAIL".to_string(),
            ],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(
            &mut cargo,
            &mut qemu,
            Some(ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE),
        );

        assert_eq!(
            qemu.success_regex,
            vec!["task stack guard page hit for .*stack-guard-page-overflow"]
        );
        assert_eq!(qemu.fail_regex, vec!["stack guard page was not hit"]);
        assert_eq!(qemu.timeout, Some(30));
    }

    #[test]
    fn arceos_rust_aarch64_qemu_config_enables_smp_for_ipi_paths() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-suit/arceos/rust");
        let qemu_path = root.join("qemu-aarch64.toml");
        let config: QemuConfig =
            toml::from_str(&std::fs::read_to_string(qemu_path).unwrap()).unwrap();
        let smp = qemu_test::smp_from_qemu_arg(&config).unwrap();
        assert!(
            smp >= 2,
            "aarch64 task-ipi, task-smp-online, and task-stack-guard-page require SMP >= 2, got \
             {smp}"
        );
    }

    #[test]
    fn arceos_rust_panic_path_qemu_uses_panic_backtrace_result_regex() {
        let mut cargo = rust_test_cargo_for_target("x86_64-unknown-none");
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(
            &mut cargo,
            &mut qemu,
            Some(ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE),
        );

        assert_eq!(
            qemu.success_regex,
            vec![r"BACKTRACE_BEGIN\b.*\bkind=panic\b"]
        );
        assert_eq!(qemu.fail_regex, vec!["ARCEOS_TEST_FAIL"]);
        assert_eq!(qemu.timeout, Some(30));
    }

    #[test]
    fn arceos_rust_lockdep_detect_qemu_uses_lockdep_result_regex() {
        let mut cargo = rust_test_cargo_for_target("x86_64-unknown-none");
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(
            &mut cargo,
            &mut qemu,
            Some(ARCEOS_RUST_LOCKDEP_DETECT_FEATURE),
        );

        assert_eq!(
            qemu.success_regex,
            vec!["lockdep: lock order inversion detected"]
        );
        assert_eq!(
            qemu.fail_regex,
            vec![r"lockdep did not report an expected .*lock order inversion"]
        );
        assert_eq!(qemu.timeout, Some(30));
    }

    #[test]
    fn arceos_rust_remote_wake_riscv_uses_single_threaded_tcg() {
        let mut cargo = rust_test_cargo_for_target("riscv64gc-unknown-none-elf");
        let mut qemu = QemuConfig::default();

        apply_rust_qemu_feature_overrides(
            &mut cargo,
            &mut qemu,
            Some("task-wait-queue-remote-wake"),
        );

        assert!(
            qemu.args
                .windows(2)
                .any(|args| args == ["-accel", "tcg,thread=single"])
        );
    }

    #[test]
    fn arceos_rust_normal_qemu_keeps_suite_result_regex() {
        let mut cargo = rust_test_cargo_for_target("x86_64-unknown-none");
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![
                r"(?i)\bpanic(?:ked)?\b".to_string(),
                "ARCEOS_TEST_FAIL".to_string(),
            ],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(&mut cargo, &mut qemu, Some("debug-backtrace"));

        assert_eq!(qemu.success_regex, vec!["ArceOS test suite run OK!"]);
        assert_eq!(
            qemu.fail_regex,
            vec![r"(?i)\bpanic(?:ked)?\b", "ARCEOS_TEST_FAIL"]
        );
        assert_eq!(qemu.timeout, Some(60));
    }

    #[test]
    fn arceos_rust_selected_case_can_miss_in_default_group_search() {
        let features = rust_qemu_features_for_list(Some("c/helloworld"), true).unwrap();
        assert!(features.is_empty());
    }

    fn rust_test_cargo_for_target(target: &str) -> Cargo {
        Cargo {
            env: Default::default(),
            target: target.to_string(),
            package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
            features: Vec::new(),
            log: None,
            extra_config: None,
            profile: None,
            disable_someboot_build_config: true,
            args: Vec::new(),
            pre_build_cmds: Vec::new(),
            post_build_cmds: Vec::new(),
            to_bin: false,
            bin: None,
            test: None,
        }
    }
}
