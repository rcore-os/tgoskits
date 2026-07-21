use super::*;

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
fn starry_qemu_default_build_config_uses_board_defconfig() {
    let path = default_qemu_build_config(Path::new("/repo"), "starry-kernel", "x86_64");

    assert_eq!(
        path,
        PathBuf::from("/repo/os/StarryOS/configs/board/qemu-x86_64.toml")
    );
}

#[test]
fn axvisor_qemu_default_build_config_uses_board_defconfig() {
    let path = default_qemu_build_config(Path::new("/repo"), "axvisor", "riscv64");

    assert_eq!(
        path,
        PathBuf::from("/repo/os/axvisor/configs/board/qemu-riscv64.toml")
    );
}

#[test]
fn starry_kernel_ktest_axstd_dev_dependency_enables_std_entry_compat() {
    let manifest_path = crate::context::workspace_root_path()
        .unwrap()
        .join("os/StarryOS/kernel/Cargo.toml");
    let manifest: toml::Table =
        toml::from_str(&fs::read_to_string(manifest_path).unwrap()).unwrap();
    let axstd = manifest["dev-dependencies"]["ax-std"].as_table().unwrap();
    let features = axstd["features"].as_array().unwrap();

    assert!(
        features
            .iter()
            .any(|feature| feature.as_str() == Some("std-compat")),
        "starry-kernel ktest uses the Rust std main(argc, argv) ABI and must enable \
         ax-std/std-compat"
    );
}

#[test]
fn system_x86_64_uefi_kernel_loader_avoids_ostool_ovmf_prebuilt() {
    let mut qemu = QemuConfig {
        args: vec!["-nographic".into()],
        uefi: true,
        ..QemuConfig::default()
    };

    apply_system_x86_64_uefi_kernel_loader(
        &mut qemu,
        Path::new("/usr/share/OVMF/OVMF_CODE.fd"),
        Path::new("/tmp/axtest.vars.fd"),
    );

    assert!(!qemu.uefi);
    assert!(qemu.to_bin);
    assert!(qemu.args.iter().any(|arg| arg.contains("OVMF_CODE.fd")));
    assert!(qemu.args.iter().any(|arg| arg.contains("axtest.vars.fd")));
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

    prepare_ktest_cargo(&mut cargo, &target, false);

    assert!(cargo.bin.is_none());
    assert_eq!(cargo.test.as_deref(), Some("kernel"));
    assert_eq!(cargo.args, vec!["--release"]);
    assert!(cargo.features.iter().any(|feature| feature == "axtest"));
    assert!(cargo.features.iter().any(|feature| feature == "extra"));
    assert!(
        cargo
            .env
            .get("CARGO_ENCODED_RUSTFLAGS")
            .is_some_and(|flags| flags.contains("cfg(axtest)"))
    );
}

#[test]
fn llvm_cov_html_args_ignore_cargo_and_rustup_sources() {
    let args = llvm_cov_html_args(
        Path::new("/repo/target/kernel.elf"),
        Path::new("/repo/coverage/kernel.profdata"),
        Path::new("/repo/coverage/kernel-html"),
    );
    let rendered = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>();

    assert!(rendered.iter().any(|arg| arg == "show"));
    assert!(
        rendered
            .iter()
            .any(|arg| arg == "-ignore-filename-regex=[/\\\\]\\.(cargo|rustup)[/\\\\]"),
        "llvm-cov HTML reports should not include Cargo registry or Rust toolchain sources: \
         {rendered:?}"
    );
}
