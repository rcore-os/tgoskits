use std::fs;

use tempfile::tempdir;

use super::discover_apps;
use crate::starry::app::{
    StarryAppKind,
    test_support::{write_benchmark_case_file, write_case_file, write_minimal_board_case},
};

#[test]
fn discovers_prebuild_apps_and_ignores_listed_names() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "codex-cli",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(
        root.path(),
        "picoclaw-cli",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(
        root.path(),
        "orangepi-5-plus-uvc",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(
        root.path(),
        "orangepi-5-plus-uvc-rknn",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    fs::write(
        root.path().join("apps/.ignore"),
        "apps/starry/orangepi-5-plus-uvc\napps/starry/orangepi-5-plus-uvc-rknn\n",
    )
    .unwrap();

    let apps = discover_apps(root.path()).unwrap();
    let names = apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>();

    assert!(names.contains(&"codex-cli"));
    assert!(names.contains(&"picoclaw-cli"));
    assert!(!names.contains(&"orangepi-5-plus-uvc"));
    assert!(!names.contains(&"orangepi-5-plus-uvc-rknn"));
}

#[test]
fn infers_qemu_and_board_app_kinds() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "codex-cli",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(
        root.path(),
        "codex-cli",
        "qemu-x86_64-codex-help.toml",
        "args = []\n",
    );
    write_minimal_board_case(root.path(), "board-demo");

    let apps = discover_apps(root.path()).unwrap();

    let board = apps.iter().find(|app| app.name == "board-demo").unwrap();
    let qemu = apps.iter().find(|app| app.name == "codex-cli").unwrap();
    assert_eq!(board.kind, StarryAppKind::Board);
    assert_eq!(qemu.kind, StarryAppKind::Qemu);
}
#[test]
fn infers_combined_qemu_and_board_app_kind() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "linux-perf",
        "qemu-aarch64.toml",
        "args = []\n",
    );
    write_minimal_board_case(root.path(), "linux-perf");

    let apps = discover_apps(root.path()).unwrap();
    let app = apps.iter().find(|app| app.name == "linux-perf").unwrap();

    assert_eq!(app.kind, StarryAppKind::Both);
}

#[test]
fn discovers_benchmark_cases_with_a_distinct_name_and_flag() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "qemu/compile-sim-bench",
        "qemu-x86_64.toml",
        "args = []\n",
    );
    write_benchmark_case_file(
        root.path(),
        "qemu/compile-sim-bench",
        "qemu-x86_64-benchmark.toml",
        "args = []\n",
    );

    let apps = discover_apps(root.path()).unwrap();

    let functional = apps
        .iter()
        .find(|app| app.name == "qemu/compile-sim-bench")
        .expect("functional smoke case must stay discoverable");
    assert_eq!(functional.kind, StarryAppKind::Qemu);
    assert!(!functional.benchmark);
    assert!(
        functional
            .case_dir
            .ends_with("apps/starry/qemu/compile-sim-bench")
    );

    let benchmark = apps
        .iter()
        .find(|app| app.name == "benchmark/qemu/compile-sim-bench")
        .expect("nightly benchmark case must be discoverable separately");
    assert_eq!(benchmark.kind, StarryAppKind::Qemu);
    assert!(benchmark.benchmark);
    assert!(
        benchmark
            .case_dir
            .ends_with("apps/benchmark/starry/qemu/compile-sim-bench")
    );
}

#[test]
fn discovers_benchmark_case_without_an_apps_starry_peer() {
    let root = tempdir().unwrap();
    fs::create_dir_all(root.path().join("apps/starry")).unwrap();
    write_benchmark_case_file(
        root.path(),
        "block-io-bench",
        "qemu-x86_64.toml",
        "args = []\n",
    );

    let apps = discover_apps(root.path()).unwrap();

    let benchmark = apps
        .iter()
        .find(|app| app.name == "benchmark/block-io-bench")
        .expect("benchmark case must be discovered");
    assert!(benchmark.benchmark);
    assert_eq!(benchmark.kind, StarryAppKind::Qemu);
    let names = apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>();
    assert!(!names.contains(&"block-io-bench"));
}
