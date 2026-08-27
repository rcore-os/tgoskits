use std::{fs, sync::Arc};

use anyhow::{Context, bail};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};
use regex::Regex;

use super::{
    ARCEOS_RUST_ALL_FEATURE, ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE,
    ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE, ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE,
    ARCEOS_RUST_LOCKDEP_DETECT_FEATURE, ARCEOS_RUST_QEMU_FEATURES,
    ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE, ARCEOS_RUST_STANDALONE_FEATURES,
    assets::test_build_args,
    discovery::discover_rust_qemu_cases,
    runner::run_prepared_qemu_groups,
    types::{ArceosRustQemuCase, PreparedArceosRustQemuCase},
};
use crate::{
    arceos::{ArceOS, build, rootfs},
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
        qemu_test::validate_test_qemu_rootfs_write_policy(&case.case.qemu_config_path, "ArceOS")?;
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
        apply_rust_qemu_feature_overrides(&mut qemu, case.feature.as_deref());
        qemu_test::apply_timeout_scale(&mut qemu);
        rootfs::prepare_default_qemu_fat32_rootfs(arceos.app.workspace_root(), &qemu)?;
        rootfs::isolate_qemu_test_rootfs(&mut qemu)?;
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

fn apply_rust_qemu_feature_overrides(qemu: &mut QemuConfig, feature: Option<&str>) {
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
        _ => {}
    }
}

fn is_lockdep_detect_feature(feature: &str) -> bool {
    matches!(feature, ARCEOS_RUST_LOCKDEP_DETECT_FEATURE)
}

fn add_cargo_feature(cargo: &mut Cargo, feature: &str) {
    // The feature comes from the explicitly selected ArceOS test case; the
    // normal suite path never adds test-specific capabilities.
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
            success_output: None,
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
        None => {
            let mut features = vec![ARCEOS_RUST_ALL_FEATURE];
            features.extend_from_slice(ARCEOS_RUST_STANDALONE_FEATURES);
            Ok(features)
        }
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
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::{
        arceos::test::{
            ARCEOS_RUST_TASK_IRQ_FEATURE, ARCEOS_RUST_TEST_PACKAGE,
            discovery::arceos_test_suit_case_qemu_config_path,
        },
        test::case::TestQemuCase,
    };

    fn rust_test_suite_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-suit/arceos/rust")
    }

    fn load_qemu_config(path: &Path) -> QemuConfig {
        toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn arceos_rust_default_run_selects_bulk_and_standalone_features() {
        let features = rust_qemu_features_for_run(None, false).unwrap();
        assert_eq!(
            features,
            vec![ARCEOS_RUST_ALL_FEATURE, ARCEOS_RUST_TASK_IRQ_FEATURE]
        );
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
            "sched-cfs",
            "sched-rr",
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
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(
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
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![
                r"(?i)\bpanic(?:ked)?\b".to_string(),
                "ARCEOS_TEST_FAIL".to_string(),
            ],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(&mut qemu, Some(ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE));

        assert_eq!(
            qemu.success_regex,
            vec!["task stack guard page hit for .*stack-guard-page-overflow"]
        );
        assert_eq!(qemu.fail_regex, vec!["stack guard page was not hit"]);
        assert_eq!(qemu.timeout, Some(30));
    }

    #[test]
    fn arceos_rust_aarch64_qemu_config_uses_gicv2_smp4_for_ipi_paths() {
        let qemu_path = rust_test_suite_root().join("qemu-aarch64.toml");
        let config = load_qemu_config(&qemu_path);
        let smp = qemu_test::smp_from_qemu_arg(&config).unwrap();
        assert_eq!(smp, 4, "aarch64 GICv2 IPI coverage requires SMP4");
        assert!(
            config
                .args
                .windows(2)
                .any(|args| args == ["-machine", "virt,gic-version=2"]),
            "aarch64 IPI coverage must exercise the GICv2 target-list path"
        );
    }

    #[test]
    fn arceos_rust_aarch64_qemu_config_converts_high_half_kernel_to_bin() {
        let qemu_path = rust_test_suite_root().join("qemu-aarch64.toml");
        let config = load_qemu_config(&qemu_path);

        assert!(
            config.to_bin,
            "the AArch64 kernel is linked at a high-half address and QEMU must load its raw BIN"
        );
    }

    #[test]
    fn arceos_rust_panic_path_qemu_uses_panic_backtrace_result_regex() {
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(&mut qemu, Some(ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE));

        assert_eq!(
            qemu.success_regex,
            vec![r"BACKTRACE_BEGIN\b.*\bkind=panic\b"]
        );
        assert_eq!(qemu.fail_regex, vec!["ARCEOS_TEST_FAIL"]);
        assert_eq!(qemu.timeout, Some(30));
    }

    #[test]
    fn arceos_rust_lockdep_detect_qemu_uses_lockdep_result_regex() {
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(&mut qemu, Some(ARCEOS_RUST_LOCKDEP_DETECT_FEATURE));

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
    fn arceos_rust_remote_wake_riscv_config_uses_single_threaded_tcg() {
        let path = arceos_test_suit_case_qemu_config_path(
            &rust_test_suite_root(),
            "riscv64",
            "task-wait-queue-remote-wake",
        )
        .unwrap();
        let qemu = load_qemu_config(&path);

        assert!(
            qemu.args
                .windows(2)
                .any(|args| args == ["-accel", "tcg,thread=single"])
        );
    }

    #[test]
    fn arceos_rust_task_ipi_riscv_config_uses_single_threaded_tcg_and_short_timeout() {
        let path =
            arceos_test_suit_case_qemu_config_path(&rust_test_suite_root(), "riscv64", "task-ipi")
                .unwrap();
        let qemu = load_qemu_config(&path);

        assert!(
            qemu.args
                .windows(2)
                .any(|args| args == ["-accel", "tcg,thread=single"])
        );
        assert_eq!(qemu.timeout, Some(15));
    }

    #[test]
    fn arceos_rust_task_ipi_non_riscv_falls_back_to_suite_config() {
        let path =
            arceos_test_suit_case_qemu_config_path(&rust_test_suite_root(), "x86_64", "task-ipi")
                .unwrap();
        let qemu = load_qemu_config(&path);

        assert!(
            !qemu
                .args
                .windows(2)
                .any(|args| args == ["-accel", "tcg,thread=single"])
        );
        assert_eq!(qemu.timeout, Some(120));
        assert_eq!(path, rust_test_suite_root().join("qemu-x86_64.toml"));
    }

    #[tokio::test]
    async fn arceos_rust_case_preparation_rejects_persistent_rootfs_policy() {
        let root = tempfile::tempdir().unwrap();
        let qemu_config_path = write_test_qemu_config(root.path(), Some("persist"));
        let mut arceos = ArceOS::new().unwrap();

        let error = prepare_rust_qemu_cases(
            &mut arceos,
            "riscv64gc-unknown-none-elf",
            vec![rust_qemu_case(qemu_config_path)],
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("cannot use `rootfs_write_policy = \"persist\"`"));
    }

    #[tokio::test]
    async fn arceos_rust_case_preparation_isolates_its_rootfs_writes() {
        let root = tempfile::tempdir().unwrap();
        let qemu_config_path = write_test_qemu_config(root.path(), None);
        let mut arceos = ArceOS::new().unwrap();

        let prepared = prepare_rust_qemu_cases(
            &mut arceos,
            "riscv64gc-unknown-none-elf",
            vec![rust_qemu_case(qemu_config_path)],
        )
        .await
        .unwrap();

        assert!(
            prepared[0].qemu.args.iter().any(|argument| {
                argument.contains("id=disk0") && argument.contains("snapshot=on")
            })
        );
    }

    #[test]
    fn arceos_rust_normal_qemu_keeps_suite_result_regex() {
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![
                r"(?i)\bpanic(?:ked)?\b".to_string(),
                "ARCEOS_TEST_FAIL".to_string(),
            ],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(&mut qemu, Some("debug-backtrace"));

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

    fn rust_qemu_case(qemu_config_path: PathBuf) -> ArceosRustQemuCase {
        ArceosRustQemuCase {
            case: TestQemuCase {
                name: "task-ipi".to_string(),
                display_name: "task-ipi".to_string(),
                case_dir: qemu_config_path.parent().unwrap().to_path_buf(),
                qemu_config_path,
                test_commands: Vec::new(),
                host_symbolize_success_regex: Vec::new(),
                host_http_server: None,
                subcases: Vec::new(),
                grouped_subcase_filter: None,
            },
            build_group: "arceos-rust".to_string(),
            build_config_path: rust_test_suite_root().join("build-riscv64gc-unknown-none-elf.toml"),
            package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
            feature: Some("task-ipi".to_string()),
        }
    }

    fn write_test_qemu_config(root: &Path, rootfs_write_policy: Option<&str>) -> PathBuf {
        let disk_path = root.join("disk.img");
        std::fs::write(&disk_path, []).unwrap();
        let policy = rootfs_write_policy
            .map(|policy| format!("rootfs_write_policy = \"{policy}\"\n"))
            .unwrap_or_default();
        let qemu_config_path = root.join("qemu-riscv64.toml");
        std::fs::write(
            &qemu_config_path,
            format!(
                r#"args = [
    "-m",
    "64M",
    "-smp",
    "4",
    "-drive",
    "id=disk0,if=none,format=raw,file={}",
]

timeout = 5
uefi = false
to_bin = false
success_regex = ["OK"]
fail_regex = ["FAIL"]
{policy}"#,
                disk_path.display()
            ),
        )
        .unwrap();
        qemu_config_path
    }
}
