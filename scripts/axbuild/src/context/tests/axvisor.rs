use super::{common::*, *};

#[test]
fn preset_overlay_selection_is_isolated_and_rejects_conflicts_before_snapshot_commit() {
    let root = tempdir().unwrap();
    let app = test_app_context(root.path());
    let overlay = root.path().join("sparse.toml");
    fs::write(
        &overlay,
        "preset = 'qemu-aarch64'\nfeatures = []\nmax_cpu_num = 0\nvm_configs = []\n",
    )
    .unwrap();
    let (selected, mut saved) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            preset: Some("qemu-aarch64".into()),
            config: Some(overlay.clone()),
            smp: Some(3),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();
    assert!(selected.config.common.features.is_empty());
    assert_eq!(selected.config.common.max_cpu_num, Some(3));
    assert_eq!(selected.runtime.cpu_capacity, 3);
    assert_eq!(selected.config_path(), Some(overlay.as_path()));
    saved.vmconfigs = vec!["old-vm.toml".into()];
    saved.qemu.qemu_config = Some("old-qemu.toml".into());
    saved.store(root.path()).unwrap();

    let (fresh, fresh_snapshot) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            preset: Some("qemu-aarch64".into()),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(fresh.config_path(), None);
    assert_eq!(fresh.smp, None);
    assert_eq!(fresh.config.common.max_cpu_num, None);
    assert!(fresh.vmconfigs.is_empty());
    assert_eq!(fresh.qemu_config, None);
    assert_eq!(fresh_snapshot.config, None);
    assert!(!root.path().join("tmp/axbuild/config").exists());

    let saved_path = snapshot_path(root.path(), AXVISOR_SNAPSHOT_FILE);
    let bytes = fs::read(&saved_path).unwrap();
    for cli in [
        AxvisorCliArgs {
            preset: Some("qemu-x86_64".into()),
            config: Some(overlay.clone()),
            ..Default::default()
        },
        AxvisorCliArgs {
            preset: Some("qemu-aarch64".into()),
            arch: Some("x86_64".into()),
            ..Default::default()
        },
    ] {
        let error = prepare_axvisor_request(&app, cli, None, None).unwrap_err();
        assert!(
            error.chain().any(|error| matches!(
                error.downcast_ref::<ax_config::ConfigError>(),
                Some(ax_config::ConfigError::SelectorConflict { .. })
            )),
            "{error:#}"
        );
        assert_eq!(fs::read(&saved_path).unwrap(), bytes);
    }
    fs::write(
        &overlay,
        "preset = 'qemu-aarch64'\ntarget = 'x86_64-unknown-none'\n",
    )
    .unwrap();
    let error = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            config: Some(overlay.clone()),
            target: Some(ax_config::boards::TARGET_AARCH64.into()),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap_err();
    assert!(
        error.chain().any(|error| matches!(
            error.downcast_ref::<ax_config::ConfigError>(),
            Some(ax_config::ConfigError::SelectorConflict { field, .. }) if field == "target"
        )),
        "{error:#}"
    );
    assert_eq!(fs::read(&saved_path).unwrap(), bytes);
    for feature in ["vmx", "svm"] {
        fs::write(
            &overlay,
            format!("preset = 'qemu-x86_64'\nfeatures = ['{feature}']\n"),
        )
        .unwrap();
        let error = prepare_axvisor_request(
            &app,
            AxvisorCliArgs {
                config: Some(overlay.clone()),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("selected from CPU capabilities at runtime"),
            "{error:#}"
        );
        assert_eq!(fs::read(&saved_path).unwrap(), bytes);
    }
}

#[test]
fn prepare_axvisor_request_prefers_cli_over_snapshot() {
    let root = tempdir().unwrap();
    write_snapshot_text(
        root.path(),
        AXVISOR_SNAPSHOT_FILE,
        r#"
config = "os/axvisor/.build.toml"
arch = "riscv64"
target = "riscv64gc-unknown-none-elf"
vmconfigs = ["tmp/snapshot-vm.toml"]

[qemu]
qemu_config = "configs/snapshot-qemu.toml"

[uboot]
uboot_config = "configs/snapshot-uboot.toml"
"#,
    )
    .unwrap();

    let custom_config = root.path().join("custom-build.toml");
    fs::write(&custom_config, "vm_configs = ['old-config-vm.toml']\n").unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            preset: None,
            config: Some(custom_config.clone()),
            arch: Some("aarch64".into()),
            target: Some(DEFAULT_AXVISOR_TARGET.into()),
            smp: Some(6),
            debug: true,
            vmconfigs: vec![
                PathBuf::from("/tmp/vm1.toml"),
                PathBuf::from("/tmp/vm2.toml"),
            ],
        },
        Some(PathBuf::from("/tmp/qemu.toml")),
        Some(PathBuf::from("/tmp/uboot.toml")),
    )
    .unwrap();

    assert_eq!(request.package, crate::axvisor::build::AXVISOR_PACKAGE);
    assert_eq!(request.arch, DEFAULT_AXVISOR_ARCH);
    assert_eq!(request.target, DEFAULT_AXVISOR_TARGET);
    assert_eq!(request.smp, Some(6));
    assert!(request.debug);
    assert_eq!(request.config_path().unwrap().to_path_buf(), custom_config);
    assert_eq!(request.qemu_config, Some(PathBuf::from("/tmp/qemu.toml")));
    assert_eq!(request.uboot_config, Some(PathBuf::from("/tmp/uboot.toml")));
    assert_eq!(
        request.vmconfigs,
        vec![
            PathBuf::from("/tmp/vm1.toml"),
            PathBuf::from("/tmp/vm2.toml")
        ]
    );
    let shown_path = root.path().join("shown.toml");
    fs::write(
        &shown_path,
        toml::to_string_pretty(&request.config).unwrap(),
    )
    .unwrap();
    let (reused, _) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            config: Some(shown_path),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(reused.config.vm_configs, request.vmconfigs);
    assert_eq!(snapshot.config, Some(PathBuf::from("custom-build.toml")));
    assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_AXVISOR_ARCH));
    assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_AXVISOR_TARGET));
    assert_eq!(snapshot.smp, Some(6));
    assert_eq!(
        snapshot.vmconfigs,
        vec![
            PathBuf::from("/tmp/vm1.toml"),
            PathBuf::from("/tmp/vm2.toml")
        ]
    );
    assert_eq!(
        snapshot.qemu.qemu_config,
        Some(PathBuf::from("/tmp/qemu.toml"))
    );
    assert_eq!(
        snapshot.uboot.uboot_config,
        Some(PathBuf::from("/tmp/uboot.toml"))
    );
}

#[test]
fn prepare_axvisor_request_uses_snapshot_when_cli_omits_values() {
    let root = tempdir().unwrap();
    write_snapshot_text(
        root.path(),
        AXVISOR_SNAPSHOT_FILE,
        r#"
config = "os/axvisor/.build.toml"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
vmconfigs = ["tmp/vm1.toml", "tmp/vm2.toml"]

[qemu]
qemu_config = "configs/qemu.toml"

[uboot]
uboot_config = "configs/uboot.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let saved_path = snapshot_path(root.path(), AXVISOR_SNAPSHOT_FILE);
    let saved = fs::read(&saved_path).unwrap();
    let error = prepare_axvisor_request(&app, AxvisorCliArgs::default(), None, None).unwrap_err();
    assert!(format!("{error:#}").contains("--preset"), "{error:#}");
    assert_eq!(fs::read(&saved_path).unwrap(), saved);
    let restored_file = root.path().join("os/axvisor/.build.toml");
    fs::create_dir_all(restored_file.parent().unwrap()).unwrap();
    fs::write(&restored_file, "").unwrap();
    let (request, snapshot) =
        prepare_axvisor_request(&app, AxvisorCliArgs::default(), None, None).unwrap();

    assert_eq!(request.arch, DEFAULT_AXVISOR_ARCH);
    assert_eq!(request.target, DEFAULT_AXVISOR_TARGET);
    assert_eq!(request.config_path(), Some(restored_file.as_path()));
    assert_eq!(snapshot.preset, None);
    assert!(!root.path().join("tmp/axbuild/config").exists());
    assert_eq!(
        request.qemu_config,
        Some(root.path().join("configs/qemu.toml"))
    );
    assert_eq!(
        request.uboot_config,
        Some(root.path().join("configs/uboot.toml"))
    );
    assert_eq!(
        request.vmconfigs,
        vec![
            root.path().join("tmp/vm1.toml"),
            root.path().join("tmp/vm2.toml")
        ]
    );
    let shown_path = root.path().join("shown.toml");
    fs::write(
        &shown_path,
        toml::to_string_pretty(&request.config).unwrap(),
    )
    .unwrap();
    let (reused, _) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            config: Some(shown_path),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(reused.config.vm_configs, request.vmconfigs);
    assert_eq!(
        snapshot.config,
        Some(PathBuf::from("os/axvisor/.build.toml"))
    );
    assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_AXVISOR_ARCH));
    assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_AXVISOR_TARGET));
    assert_eq!(
        snapshot.vmconfigs,
        vec![PathBuf::from("tmp/vm1.toml"), PathBuf::from("tmp/vm2.toml")]
    );
    assert_eq!(
        snapshot.uboot.uboot_config,
        Some(PathBuf::from("configs/uboot.toml"))
    );
}

#[test]
fn prepare_axvisor_request_cli_arch_drops_stale_runtime_paths() {
    let root = tempdir().unwrap();
    write_snapshot_text(
        root.path(),
        AXVISOR_SNAPSHOT_FILE,
        r#"
config = "os/axvisor/.build.toml"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
vmconfigs = ["tmp/snapshot-vm.toml"]

[qemu]
qemu_config = "configs/qemu-aarch64.toml"

[uboot]
uboot_config = "configs/uboot-aarch64.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            preset: None,
            config: None,
            arch: Some("x86_64".into()),
            target: None,
            smp: None,
            debug: false,
            vmconfigs: vec![],
        },
        None,
        None,
    )
    .unwrap();

    assert_eq!(request.arch, "x86_64");
    assert_eq!(request.target, "x86_64-unknown-none");
    assert_eq!(request.qemu_config, None);
    assert_eq!(request.uboot_config, None);
    assert_eq!(snapshot.qemu.qemu_config, None);
    assert_eq!(snapshot.uboot.uboot_config, None);
}

#[test]
fn prepare_axvisor_request_cli_arch_ignores_stale_snapshot_config_target() {
    let root = tempdir().unwrap();
    write_snapshot_text(
        root.path(),
        AXVISOR_SNAPSHOT_FILE,
        r#"
config = "os/axvisor/.build-loongarch64-unknown-none-softfloat.toml"
arch = "loongarch64"
target = "loongarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.path().join("os/axvisor")).unwrap();
    fs::write(
        root.path()
            .join("os/axvisor/.build-loongarch64-unknown-none-softfloat.toml"),
        r#"
target = "loongarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            preset: None,
            config: None,
            arch: Some("x86_64".into()),
            target: None,
            smp: None,
            debug: false,
            vmconfigs: vec![],
        },
        None,
        None,
    )
    .unwrap();

    assert_eq!(request.arch, "x86_64");
    assert_eq!(request.target, "x86_64-unknown-none");
    assert_eq!(request.config_path(), None);
    assert_eq!(snapshot.config, None);
    assert_eq!(request.source.preset(), snapshot.preset.as_deref());
    assert!(snapshot.preset.is_some());
    assert_eq!(snapshot.arch.as_deref(), Some("x86_64"));
    assert_eq!(snapshot.target.as_deref(), Some("x86_64-unknown-none"));
}

#[test]
fn prepare_axvisor_request_explicit_config_drops_snapshot_runtime_and_vmconfigs() {
    let root = tempdir().unwrap();
    let explicit = root
        .path()
        .join("test-suit/axvisor/normal/qemu/build-x86_64-unknown-none.toml");
    fs::create_dir_all(explicit.parent().unwrap()).unwrap();
    fs::write(
        &explicit,
        r#"
target = "x86_64-unknown-none"
features = []
log = "Info"
max_cpu_num = 2
vm_configs = ["os/axvisor/configs/vms/qemu/x86_64/arceos-smp1.toml"]
[runtime]
task_stack_size = 0x50000
scheduler_tick_ms = 5
"#,
    )
    .unwrap();
    write_snapshot_text(
        root.path(),
        AXVISOR_SNAPSHOT_FILE,
        r#"
arch = "x86_64"
target = "x86_64-unknown-none"
smp = 8
vmconfigs = ["os/axvisor/configs/vms/qemu/x86_64/linux-smp1.toml"]
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            preset: None,
            config: Some(explicit.clone()),
            arch: None,
            target: None,
            smp: None,
            debug: false,
            vmconfigs: vec![],
        },
        None,
        None,
    )
    .unwrap();

    assert_eq!(request.config_path().unwrap().to_path_buf(), explicit);
    assert!(request.vmconfigs.is_empty());
    assert!(snapshot.vmconfigs.is_empty());
    assert_eq!(request.smp, None);
    assert_eq!(snapshot.smp, None);
    assert_eq!(request.config.common.max_cpu_num, Some(2));
    assert_eq!(request.runtime.cpu_capacity, 2);
    assert_eq!(request.runtime.task_stack_size, 0x50000);
    assert_eq!(request.runtime.scheduler_tick_ms, 5);

    let snapshot_path = snapshot_path(root.path(), AXVISOR_SNAPSHOT_FILE);
    let saved_snapshot = fs::read(&snapshot_path).unwrap();
    let contents = fs::read_to_string(&explicit).unwrap();
    fs::write(
        &explicit,
        contents.replace("max_cpu_num = 2", "max_cpu_num = 0"),
    )
    .unwrap();
    let cli = |smp| AxvisorCliArgs {
        preset: None,
        config: Some(explicit.clone()),
        smp,
        ..Default::default()
    };
    let (overridden, _) = prepare_axvisor_request(&app, cli(Some(4)), None, None).unwrap();
    assert_eq!(overridden.runtime.cpu_capacity, 4);
    assert_eq!(overridden.config.common.max_cpu_num, Some(4));
    let error = prepare_axvisor_request(&app, cli(None), None, None).unwrap_err();
    assert!(
        error.chain().any(|cause| matches!(
            cause.downcast_ref::<ax_config::ConfigError>(),
            Some(ax_config::ConfigError::InvalidValue { value, .. }) if value == "0"
        )),
        "{error:#}"
    );
    assert_eq!(fs::read(snapshot_path).unwrap(), saved_snapshot);
}
