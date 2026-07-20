use std::fs;

use tempfile::tempdir;

use super::selected_apps;
use crate::starry::app::{ArgsAppQemu, StarryAppKind, test_support::write_case_file};

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
        caps: Vec::new(),
        arch: Some("x86_64".to_string()),
        qemu_config: None,
        debug: false,
    };

    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
    let names = apps.into_iter().map(|app| app.name).collect::<Vec<_>>();

    assert_eq!(names, vec!["qemu/apk-curl"]);
}

#[test]
fn all_qemu_selection_uses_starry_default_arch_without_an_arch_argument() {
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
        caps: Vec::new(),
        arch: None,
        qemu_config: None,
        debug: false,
    };

    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
    let names = apps.into_iter().map(|app| app.name).collect::<Vec<_>>();

    assert_eq!(names, vec!["qemu/apt"]);
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
        caps: Vec::new(),
        arch: Some("loongarch64".to_string()),
        qemu_config: None,
        debug: false,
    };

    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
    let names = apps.into_iter().map(|app| app.name).collect::<Vec<_>>();

    assert_eq!(names, vec!["apache"]);
}

#[test]
fn selected_qemu_case_allows_ignored_app_when_explicit() {
    let root = tempdir().unwrap();
    write_case_file(root.path(), "gdb-smoke", "qemu-riscv64.toml", "args = []\n");
    fs::write(root.path().join("apps/.ignore"), "apps/starry/gdb-smoke\n").unwrap();
    let args = ArgsAppQemu {
        all: false,
        test_case: Some("gdb-smoke".to_string()),
        caps: Vec::new(),
        arch: Some("riscv64".to_string()),
        qemu_config: None,
        debug: false,
    };

    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
    let names = apps.into_iter().map(|app| app.name).collect::<Vec<_>>();

    assert_eq!(names, vec!["gdb-smoke"]);
}

#[test]
fn selected_qemu_case_allows_ignored_nested_app_when_explicit() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "k230-qemu/qemu-k230/kpu-smoke",
        "qemu-riscv64.toml",
        "args = []\n",
    );
    fs::write(root.path().join("apps/.ignore"), "apps/starry/k230-qemu\n").unwrap();
    let args = ArgsAppQemu {
        all: false,
        test_case: Some("k230-qemu/qemu-k230/kpu-smoke".to_string()),
        caps: Vec::new(),
        arch: Some("riscv64".to_string()),
        qemu_config: None,
        debug: false,
    };

    let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
    let names = apps.into_iter().map(|app| app.name).collect::<Vec<_>>();

    assert_eq!(names, vec!["k230-qemu/qemu-k230/kpu-smoke"]);
}
