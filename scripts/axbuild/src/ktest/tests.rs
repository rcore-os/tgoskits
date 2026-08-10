use std::process::Command;

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
fn starry_kernel_ktest_axstd_dev_dependency_keeps_freestanding_entry_contract() {
    let manifest_path = crate::context::workspace_root_path()
        .unwrap()
        .join("os/StarryOS/kernel/Cargo.toml");
    let manifest: toml::Table =
        toml::from_str(&fs::read_to_string(manifest_path).unwrap()).unwrap();
    let axstd = manifest["dev-dependencies"]["ax-std"].as_table().unwrap();
    let features = axstd["features"].as_array().unwrap();

    assert_eq!(axstd["default-features"].as_bool(), Some(false));
    assert!(
        features
            .iter()
            .any(|feature| feature.as_str() == Some("alloc"))
    );
    assert!(
        features
            .iter()
            .all(|feature| !matches!(feature.as_str(), Some("std-compat" | "tls"))),
        "Starry ktest targets share the bare no_std/no-TLS kernel entry contract"
    );
}

#[test]
fn workspace_bindgen_consumers_use_minimal_host_features() {
    let workspace_root = crate::context::workspace_root_path().unwrap();
    let workspace_manifest: toml::Table =
        toml::from_str(&fs::read_to_string(workspace_root.join("Cargo.toml")).unwrap()).unwrap();
    let bindgen = workspace_manifest["workspace"]["dependencies"]["bindgen"]
        .as_table()
        .expect("workspace bindgen dependency must declare an explicit feature contract");
    let features = bindgen["features"].as_array().unwrap();

    assert_eq!(bindgen["default-features"].as_bool(), Some(false));
    assert_eq!(
        features
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>(),
        ["runtime"],
        "workspace bindgen consumers only need runtime libclang loading"
    );

    for manifest_path in [
        "os/arceos/api/arceos_posix_api/Cargo.toml",
        "os/arceos/ulib/axlibc/Cargo.toml",
    ] {
        let manifest: toml::Table =
            toml::from_str(&fs::read_to_string(workspace_root.join(manifest_path)).unwrap())
                .unwrap();
        let bindgen = manifest["build-dependencies"]["bindgen"]
            .as_table()
            .unwrap();

        assert_eq!(bindgen["workspace"].as_bool(), Some(true));
        assert!(
            bindgen.get("features").is_none(),
            "{manifest_path} must inherit the workspace bindgen feature contract"
        );
    }
}

#[test]
fn starry_kernel_ktest_target_log_features_remain_no_std() {
    let workspace_root = crate::context::workspace_root_path().unwrap();

    for target in [
        "x86_64-unknown-none",
        "riscv64gc-unknown-none-elf",
        "aarch64-unknown-none-softfloat",
        "loongarch64-unknown-none-softfloat",
    ] {
        let output = Command::new(env!("CARGO"))
            .current_dir(&workspace_root)
            .args([
                "tree",
                "--locked",
                "--package",
                "starry-kernel",
                "--target",
                target,
                "--features",
                "axtest",
                "--edges",
                "normal,dev",
                "--invert",
                "log",
                "--depth",
                "0",
                "--format",
                "{p}|{f}",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to resolve Starry ktest target graph for {target}: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let resolved_log = String::from_utf8(output.stdout).unwrap();
        let (_, features) = resolved_log
            .trim()
            .split_once('|')
            .expect("cargo tree must report the resolved log feature set");
        assert!(
            features.split(',').all(|feature| feature != "std"),
            "{target} must compile log without std, resolved features: {features}"
        );
    }
}

#[test]
fn x86_64_uefi_kernel_loader_uses_explicit_cached_pflash() {
    let mut qemu = QemuConfig {
        args: vec!["-nographic".into()],
        uefi: true,
        ..QemuConfig::default()
    };

    apply_x86_64_uefi_kernel_loader(
        &mut qemu,
        Path::new("/cache/ovmf/x64/code.fd"),
        Path::new("/tmp/axtest.vars.fd"),
    );

    assert!(!qemu.uefi);
    assert!(qemu.to_bin);
    assert!(
        qemu.args
            .iter()
            .any(|arg| arg.contains("/cache/ovmf/x64/code.fd"))
    );
    assert!(
        qemu.args
            .iter()
            .any(|arg| arg.contains("/tmp/axtest.vars.fd"))
    );
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

    prepare_ktest_cargo(&mut cargo, &target, true);

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
fn llvm_cov_html_args_ignore_non_workspace_sources_and_target_outputs() {
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
            .any(|arg| arg == "-ignore-filename-regex=[/\\\\](\\.(cargo|rustup)|target)[/\\\\]"),
        "llvm-cov HTML reports should not include Cargo registry, Rust toolchain, or target \
         output sources: {rendered:?}"
    );
}
