use std::{
    fs,
    path::{Path, PathBuf},
};

use cargo_metadata::Package;

use super::{
    git::{SyncLintSelection, select_sync_lint_files_for_paths, workspace_rust_source_files},
    parser::analyze_file,
    rules::{Finding, Rule},
};

fn package(root: &Path, name: &str) -> Package {
    let value = serde_json::json!({
        "name": name,
        "version": "0.1.0",
        "id": format!("{name} 0.1.0 (path+file://{}/crates/{name})", root.display()),
        "license": null,
        "license_file": null,
        "description": null,
        "source": null,
        "dependencies": [],
        "targets": [{
            "kind": ["lib"],
            "crate_types": ["lib"],
            "name": name,
            "src_path": format!("{}/crates/{name}/src/lib.rs", root.display()),
            "edition": "2021",
            "doc": true,
            "doctest": true,
            "test": true
        }],
        "features": serde_json::Map::new(),
        "manifest_path": format!("{}/crates/{name}/Cargo.toml", root.display()),
        "metadata": null,
        "publish": null,
        "authors": [],
        "categories": [],
        "keywords": [],
        "readme": null,
        "repository": null,
        "homepage": null,
        "documentation": null,
        "edition": "2021",
        "links": null,
        "default_run": null,
        "rust_version": null
    });
    serde_json::from_value(value).unwrap()
}

fn findings(source: &str) -> Vec<Finding> {
    let syntax = syn::parse_file(source).unwrap();
    analyze_file(Path::new("test.rs"), source, &syntax).findings
}

#[test]
fn incremental_selection_keeps_changed_rust_files() {
    let root = tempfile::tempdir().unwrap();
    let src_dir = root.path().join("crates/alpha/src");
    fs::create_dir_all(&src_dir).unwrap();
    let lib = src_dir.join("lib.rs");
    fs::write(&lib, "").unwrap();
    let packages = vec![package(root.path(), "alpha")];

    let selection = select_sync_lint_files_for_paths(
        root.path(),
        &packages,
        [PathBuf::from("crates/alpha/src/lib.rs")],
    )
    .unwrap();

    assert_eq!(selection, SyncLintSelection::Files(vec![lib]));
}

#[test]
fn incremental_selection_skips_changed_non_rust_package_files() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("crates/alpha/src")).unwrap();
    let packages = vec![package(root.path(), "alpha")];

    let selection = select_sync_lint_files_for_paths(
        root.path(),
        &packages,
        [PathBuf::from("crates/alpha/README.md")],
    )
    .unwrap();

    assert_eq!(selection, SyncLintSelection::Files(Vec::new()));
}

#[test]
fn incremental_selection_skips_changed_non_rust_global_files() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("crates/alpha/src")).unwrap();
    let packages = vec![package(root.path(), "alpha")];

    let selection =
        select_sync_lint_files_for_paths(root.path(), &packages, [PathBuf::from("Cargo.lock")])
            .unwrap();

    assert_eq!(selection, SyncLintSelection::Files(Vec::new()));
}

#[test]
fn incremental_selection_falls_back_for_global_rust_files() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("crates/alpha/src")).unwrap();
    fs::write(root.path().join("build.rs"), "").unwrap();
    let packages = vec![package(root.path(), "alpha")];

    let selection =
        select_sync_lint_files_for_paths(root.path(), &packages, [PathBuf::from("build.rs")])
            .unwrap();

    assert!(matches!(
        selection,
        SyncLintSelection::All { reason: Some(reason) } if reason.contains("build.rs")
    ));
}

#[test]
fn workspace_source_files_are_deduplicated_for_nested_packages() {
    let root = tempfile::tempdir().unwrap();
    let alpha_src = root.path().join("crates/alpha/src");
    let beta_src = root.path().join("crates/alpha/beta/src");
    fs::create_dir_all(&alpha_src).unwrap();
    fs::create_dir_all(&beta_src).unwrap();
    let alpha_lib = alpha_src.join("lib.rs");
    let beta_lib = beta_src.join("lib.rs");
    fs::write(&alpha_lib, "").unwrap();
    fs::write(&beta_lib, "").unwrap();
    let packages = vec![
        package(root.path(), "alpha"),
        package(root.path(), "alpha/beta"),
    ];

    let files = workspace_rust_source_files(&packages).unwrap();

    assert_eq!(files, vec![beta_lib, alpha_lib]);
}

#[test]
fn reports_relaxed_wait_condition_in_wait_until() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicUsize, Ordering};

fn demo(wq: WaitQueue, counter: &AtomicUsize) {
    wq.wait_until(|| counter.load(Ordering::Relaxed) == 1);
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::WaitCondition)
    );
}

#[test]
fn reports_relaxed_wait_condition_in_blocking_loop() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool) {
    while !flag.load(Ordering::Relaxed) {
        core::hint::spin_loop();
    }
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::WaitCondition)
    );
}

#[test]
fn reports_relaxed_publish_before_notify() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Relaxed);
    wq.notify_all();
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::PublishBeforeNotify)
    );
}

#[test]
fn reports_relaxed_publish_before_ipi() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, target: IpiTarget) {
    flag.store(true, Ordering::Relaxed);
    ax_hal::irq::send_ipi(IPI_IRQ, target);
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::PublishBeforeNotify)
    );
}

#[test]
fn reports_relaxed_publish_before_waker() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, waker: &core::task::Waker) {
    flag.store(true, Ordering::Relaxed);
    waker.wake_by_ref();
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::PublishBeforeNotify)
    );
}

#[test]
fn reports_relaxed_publish_before_task_wake() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, task: &AxTaskRef) {
    flag.store(true, Ordering::Relaxed);
    ax_task::wake_task(task);
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::PublishBeforeNotify)
    );
}

#[test]
fn reports_relaxed_publish_before_signal_wake_entrypoint() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, tid: Pid, sig: SignalInfo) -> AxResult<()> {
    flag.store(true, Ordering::Relaxed);
    send_signal_to_thread(None, tid, Some(sig))
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::PublishBeforeNotify)
    );
}

#[test]
fn ignores_release_wait_conditions() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicUsize, Ordering};

fn demo(wq: WaitQueue, counter: &AtomicUsize) {
    wq.wait_until(|| counter.load(Ordering::Acquire) == 1);
}
"#,
    );

    assert!(findings.is_empty());
}

#[test]
fn respects_ignore_comment() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicUsize, Ordering};

fn demo(wq: WaitQueue, counter: &AtomicUsize) {
    // sync-lint: ignore suspicious_relaxed_wait_condition
    wq.wait_until(|| counter.load(Ordering::Relaxed) == 1);
}
"#,
    );

    assert!(findings.is_empty());
}

#[test]
fn reports_relaxed_mixed_ordering_for_sync_wait_variable() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Relaxed);
    wq.wait_until(|| flag.load(Ordering::Acquire));
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::MixedOrdering)
    );
}

#[test]
fn reports_relaxed_mixed_ordering_after_publish_notify() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Release);
    wq.notify_all();
    let _ = flag.load(Ordering::Relaxed);
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::MixedOrdering)
    );
}

#[test]
fn reports_relaxed_mixed_ordering_for_parenthesized_receiver() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Relaxed);
    wq.wait_until(|| (flag).load(Ordering::Acquire));
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::MixedOrdering)
    );
}

#[test]
fn reports_mixed_ordering_for_receiver_field_across_methods() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

struct Runtime {
    ready: AtomicBool,
    wq: WaitQueue,
}

impl Runtime {
    fn wait(&self) {
        self.wq.wait_until(|| self.ready.load(Ordering::Acquire));
    }

    fn check(&self) {
        let _ = self.ready.load(Ordering::Relaxed);
    }
}
"#,
    );

    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::MixedOrdering)
    );
}

#[test]
fn keeps_same_receiver_field_names_separate_across_impl_types() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

struct Runtime {
    ready: AtomicBool,
    wq: WaitQueue,
}

struct Stats {
    ready: AtomicBool,
}

impl Runtime {
    fn wait(&self) {
        self.wq.wait_until(|| self.ready.load(Ordering::Acquire));
    }
}

impl Stats {
    fn check(&self) {
        let _ = self.ready.load(Ordering::Relaxed);
    }
}
"#,
    );

    assert!(findings.is_empty());
}

#[test]
fn ignores_mixed_ordering_for_different_function_bindings_with_same_name() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn sync_path(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Relaxed);
    wq.wait_until(|| flag.load(Ordering::Acquire));
}

fn stats_path(flag: &AtomicBool) {
    let _ = flag.load(Ordering::Relaxed);
}
"#,
    );

    let mixed = findings
        .iter()
        .filter(|finding| finding.rule == Rule::MixedOrdering)
        .collect::<Vec<_>>();

    assert_eq!(mixed.len(), 1);
}

#[test]
fn ignores_mixed_ordering_for_shadowed_binding_in_inner_scope() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Relaxed);
    wq.wait_until(|| flag.load(Ordering::Acquire));

    {
        let flag = AtomicBool::new(false);
        let _ = flag.load(Ordering::Relaxed);
    }
}
"#,
    );

    let mixed = findings
        .iter()
        .filter(|finding| finding.rule == Rule::MixedOrdering)
        .collect::<Vec<_>>();

    assert_eq!(mixed.len(), 1);
}

#[test]
fn ignores_mixed_ordering_without_sync_intent() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicU64, Ordering};

struct PollFrequencyController {
    consecutive_idle: AtomicU64,
}

impl PollFrequencyController {
    fn current_interval(&self) -> u64 {
        self.consecutive_idle.load(Ordering::Relaxed)
    }

    fn on_event(&self) {
        self.consecutive_idle.store(0, Ordering::Release);
    }
}
"#,
    );

    assert!(findings.is_empty());
}

#[test]
fn ignores_compare_exchange_failure_ordering() {
    let findings = findings(
        r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool) {
    while flag.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }

    let _ = flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed);
}
"#,
    );

    assert!(findings.is_empty());
}
