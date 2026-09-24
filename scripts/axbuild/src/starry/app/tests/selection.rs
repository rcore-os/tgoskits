use std::fs;

use tempfile::tempdir;

use super::selected_apps;
use crate::starry::app::{
    ArgsAppQemu, StarryAppKind,
    test_support::{write_benchmark_case_file, write_case_file},
};

#[test]
fn all_qemu_selection_skips_apps_without_matching_arch_config() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "qemu/apk-curl",
        "qemu-x86_64.toml",
        "args = []\n",
    );
    write_case_file(root.path(), "qemu/apt", "qemu-riscv64.toml", "args = []\n");
    let args = ArgsAppQemu {
        all: true,
        test_case: None,
        nixos_case: None,
        all_nixos_cases: false,
        list_nixos_cases: false,
        caps: Vec::new(),
        arch: Some("x86_64".to_string()),
        qemu_config: None,
        debug: false,
    };
    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
    let names = apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>();

    assert!(names.contains(&"qemu/apk-curl"));
    assert!(!names.contains(&"qemu/apt"));
}

#[test]
fn all_qemu_selection_skips_ignored_nested_app() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "ebpf/kret",
        "qemu-loongarch64.toml",
        "args = []\n",
    );
    write_case_file(
        root.path(),
        "apache",
        "qemu-loongarch64.toml",
        "args = []\n",
    );
    fs::write(root.path().join("apps/.ignore"), "apps/starry/ebpf/kret\n").unwrap();
    let args = ArgsAppQemu {
        all: true,
        test_case: None,
        nixos_case: None,
        all_nixos_cases: false,
        list_nixos_cases: false,
        caps: Vec::new(),
        arch: Some("loongarch64".to_string()),
        qemu_config: None,
        debug: false,
    };

    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
    let names = apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>();

    assert!(names.contains(&"apache"));
    assert!(!names.contains(&"ebpf/kret"));
}

#[test]
fn selected_qemu_case_allows_ignored_app_when_explicit() {
    let root = tempdir().unwrap();
    write_case_file(root.path(), "gdb-smoke", "qemu-riscv64.toml", "args = []\n");
    fs::write(root.path().join("apps/.ignore"), "apps/starry/gdb-smoke\n").unwrap();
    let args = ArgsAppQemu {
        all: false,
        test_case: Some("gdb-smoke".to_string()),
        nixos_case: None,
        all_nixos_cases: false,
        list_nixos_cases: false,
        caps: Vec::new(),
        arch: Some("riscv64".to_string()),
        qemu_config: None,
        debug: false,
    };

    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
    let names = apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>();

    assert!(names.contains(&"gdb-smoke"));
}

#[test]
fn selected_qemu_case_accepts_combined_app() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "linux-perf",
        "qemu-aarch64.toml",
        "args = []\n",
    );
    write_case_file(
        root.path(),
        "linux-perf",
        "board-orangepi-5-plus.toml",
        "args = []\n",
    );
    write_case_file(root.path(), "linux-perf", "init.sh", "#!/bin/sh\n");
    let args = ArgsAppQemu {
        all: false,
        test_case: Some("linux-perf".to_string()),
        nixos_case: None,
        all_nixos_cases: false,
        list_nixos_cases: false,
        caps: Vec::new(),
        arch: Some("aarch64".to_string()),
        qemu_config: None,
        debug: false,
    };

    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();

    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].kind, StarryAppKind::Both);
}

#[test]
fn all_qemu_selection_skips_nightly_benchmark_cases() {
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
    let args = ArgsAppQemu {
        all: true,
        test_case: None,
        nixos_case: None,
        all_nixos_cases: false,
        list_nixos_cases: false,
        caps: Vec::new(),
        arch: Some("x86_64".to_string()),
        qemu_config: None,
        debug: false,
    };

    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
    let names = apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>();

    assert!(names.contains(&"qemu/compile-sim-bench"));
    assert!(!names.contains(&"benchmark/qemu/compile-sim-bench"));
}

#[test]
fn selected_benchmark_case_resolves_through_the_benchmark_prefix() {
    let root = tempdir().unwrap();
    fs::create_dir_all(root.path().join("apps/starry")).unwrap();
    write_benchmark_case_file(
        root.path(),
        "qemu/compile-sim-bench",
        "qemu-x86_64-benchmark.toml",
        "args = []\n",
    );
    let args = ArgsAppQemu {
        all: false,
        test_case: Some("benchmark/qemu/compile-sim-bench".to_string()),
        nixos_case: None,
        all_nixos_cases: false,
        list_nixos_cases: false,
        caps: Vec::new(),
        arch: Some("x86_64".to_string()),
        qemu_config: None,
        debug: false,
    };

    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();

    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name, "benchmark/qemu/compile-sim-bench");
    assert!(apps[0].benchmark);
    assert!(
        apps[0]
            .case_dir
            .ends_with("apps/benchmark/starry/qemu/compile-sim-bench")
    );
}
