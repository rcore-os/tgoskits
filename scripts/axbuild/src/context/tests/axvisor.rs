use super::{common::*, *};

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
plat_dyn = false
vmconfigs = ["tmp/snapshot-vm.toml"]

[qemu]
qemu_config = "configs/snapshot-qemu.toml"

[uboot]
uboot_config = "configs/snapshot-uboot.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            config: Some(PathBuf::from("/tmp/custom-build.toml")),
            arch: Some("aarch64".into()),
            target: Some(DEFAULT_AXVISOR_TARGET.into()),
            plat_dyn: Some(true),
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
    assert_eq!(request.plat_dyn, Some(true));
    assert_eq!(request.smp, Some(6));
    assert!(request.debug);
    assert_eq!(
        request.build_info_path,
        PathBuf::from("/tmp/custom-build.toml")
    );
    assert_eq!(request.qemu_config, Some(PathBuf::from("/tmp/qemu.toml")));
    assert_eq!(request.uboot_config, Some(PathBuf::from("/tmp/uboot.toml")));
    assert_eq!(
        request.vmconfigs,
        vec![
            PathBuf::from("/tmp/vm1.toml"),
            PathBuf::from("/tmp/vm2.toml")
        ]
    );
    assert_eq!(
        snapshot.config,
        Some(PathBuf::from("/tmp/custom-build.toml"))
    );
    assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_AXVISOR_ARCH));
    assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_AXVISOR_TARGET));
    assert_eq!(snapshot.plat_dyn, Some(true));
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

    let (request, snapshot) =
        prepare_axvisor_request(&app, AxvisorCliArgs::default(), None, None).unwrap();

    assert_eq!(request.arch, DEFAULT_AXVISOR_ARCH);
    assert_eq!(request.target, DEFAULT_AXVISOR_TARGET);
    assert_eq!(request.plat_dyn, None);
    assert_eq!(
        request.build_info_path,
        root.path()
            .join("tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml")
    );
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
    assert_eq!(
        snapshot.config,
        Some(PathBuf::from(
            "tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml"
        ))
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
fn prepare_axvisor_request_resolves_target_from_arch() {
    let root = tempdir().unwrap();
    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            config: None,
            arch: Some("x86_64".into()),
            target: None,
            plat_dyn: None,
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
    assert_eq!(
        request.build_info_path,
        root.path()
            .join("tmp/axbuild/config/axvisor/build-x86_64-unknown-none.toml")
    );
    assert_eq!(snapshot.arch.as_deref(), Some("x86_64"));
    assert_eq!(snapshot.target.as_deref(), Some("x86_64-unknown-none"));
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
            config: None,
            arch: Some("x86_64".into()),
            target: None,
            plat_dyn: None,
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
            config: None,
            arch: Some("x86_64".into()),
            target: None,
            plat_dyn: None,
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
    assert_eq!(
        request.build_info_path,
        root.path()
            .join("tmp/axbuild/config/axvisor/build-x86_64-unknown-none.toml")
    );
    assert_eq!(
        snapshot.config,
        Some(PathBuf::from(
            "tmp/axbuild/config/axvisor/build-x86_64-unknown-none.toml"
        ))
    );
    assert_eq!(snapshot.arch.as_deref(), Some("x86_64"));
    assert_eq!(snapshot.target.as_deref(), Some("x86_64-unknown-none"));
}

#[test]
fn prepare_axvisor_request_rewrites_stale_generated_snapshot_config_path() {
    let root = tempdir().unwrap();
    write_snapshot_text(
        root.path(),
        AXVISOR_SNAPSHOT_FILE,
        r#"
config = "os/axvisor/.build-riscv64gc-unknown-none-elf.toml"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) =
        prepare_axvisor_request(&app, AxvisorCliArgs::default(), None, None).unwrap();

    assert_eq!(
        request.build_info_path,
        root.path()
            .join("tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml")
    );
    assert_eq!(
        snapshot.config,
        Some(PathBuf::from(
            "tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml"
        ))
    );
}

#[test]
fn prepare_axvisor_request_explicit_config_drops_snapshot_vmconfigs() {
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
vm_configs = ["os/axvisor/configs/vms/qemu/x86_64/arceos-smp1.toml"]
"#,
    )
    .unwrap();
    write_snapshot_text(
        root.path(),
        AXVISOR_SNAPSHOT_FILE,
        r#"
arch = "x86_64"
target = "x86_64-unknown-none"
vmconfigs = ["os/axvisor/configs/vms/qemu/x86_64/linux-vmx-smp1.toml"]
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_axvisor_request(
        &app,
        AxvisorCliArgs {
            config: Some(explicit.clone()),
            arch: None,
            target: None,
            plat_dyn: None,
            smp: None,
            debug: false,
            vmconfigs: vec![],
        },
        None,
        None,
    )
    .unwrap();

    assert_eq!(request.build_info_path, explicit);
    assert!(request.vmconfigs.is_empty());
    assert!(snapshot.vmconfigs.is_empty());
}
