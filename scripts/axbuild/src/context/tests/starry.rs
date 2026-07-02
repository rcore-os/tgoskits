use super::{common::*, *};

#[test]
fn starry_snapshot_load_returns_default_when_missing() {
    let root = tempdir().unwrap();
    let snapshot = StarryCommandSnapshot::load(root.path()).unwrap();
    assert_eq!(snapshot, StarryCommandSnapshot::default());
}

#[test]
fn starry_snapshot_store_round_trips() {
    let root = tempdir().unwrap();
    let snapshot = StarryCommandSnapshot {
        arch: Some(DEFAULT_STARRY_ARCH.into()),
        target: Some(DEFAULT_STARRY_TARGET.into()),
        smp: None,
        config: Some(PathBuf::from(
            "tmp/axbuild/config/starryos/build-riscv64gc-unknown-none-elf.toml",
        )),
        qemu: StarryQemuSnapshot {
            qemu_config: Some(PathBuf::from("configs/qemu.toml")),
        },
        uboot: StarryUbootSnapshot {
            uboot_config: Some(PathBuf::from("configs/uboot.toml")),
        },
    };

    let path = snapshot.store(root.path()).unwrap();
    let loaded = StarryCommandSnapshot::load(root.path()).unwrap();

    assert_eq!(path, snapshot_path(root.path(), STARRY_SNAPSHOT_FILE));
    assert_eq!(loaded, snapshot);
}

#[test]
fn prepare_starry_request_prefers_cli_over_snapshot() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    write_snapshot_text(
        root.path(),
        STARRY_SNAPSHOT_FILE,
        r#"
arch = "riscv64"
target = "riscv64gc-unknown-none-elf"

[qemu]
qemu_config = "configs/snapshot-qemu.toml"

[uboot]
uboot_config = "configs/snapshot-uboot.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_starry_request(
        &app,
        StarryCliArgs {
            config: Some(PathBuf::from("/tmp/starry-build.toml")),
            arch: Some("aarch64".into()),
            target: Some("aarch64-unknown-none-softfloat".into()),
            smp: Some(4),
            debug: true,
        },
        Some(PathBuf::from("/tmp/qemu.toml")),
        None,
    )
    .unwrap();

    assert_eq!(request.package, STARRY_PACKAGE);
    assert_eq!(request.arch, "aarch64");
    assert_eq!(request.target, "aarch64-unknown-none-softfloat");
    assert_eq!(request.smp, Some(4));
    assert!(request.debug);
    assert_eq!(
        request.build_info_path,
        PathBuf::from("/tmp/starry-build.toml")
    );
    assert_eq!(request.qemu_config, Some(PathBuf::from("/tmp/qemu.toml")));
    assert_eq!(request.uboot_config, None);
    assert_eq!(snapshot.arch.as_deref(), Some("aarch64"));
    assert_eq!(
        snapshot.target.as_deref(),
        Some("aarch64-unknown-none-softfloat")
    );
    assert_eq!(snapshot.smp, Some(4));
    assert_eq!(
        snapshot.config,
        Some(PathBuf::from("/tmp/starry-build.toml"))
    );
    assert_eq!(
        snapshot.qemu.qemu_config,
        Some(PathBuf::from("/tmp/qemu.toml"))
    );
    assert_eq!(snapshot.uboot.uboot_config, None);
}

#[test]
fn prepare_starry_request_uses_snapshot_and_default_arch() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    write_snapshot_text(
        root.path(),
        STARRY_SNAPSHOT_FILE,
        r#"
[qemu]
qemu_config = "configs/qemu.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) =
        prepare_starry_request(&app, StarryCliArgs::default(), None, None).unwrap();

    assert_eq!(request.package, STARRY_PACKAGE);
    assert_eq!(request.arch, DEFAULT_STARRY_ARCH);
    assert_eq!(request.target, DEFAULT_STARRY_TARGET);
    assert_eq!(
        request.build_info_path,
        root.path()
            .join("tmp/axbuild/config/starryos/build-riscv64gc-unknown-none-elf.toml")
    );
    assert_eq!(
        request.qemu_config,
        Some(root.path().join("configs/qemu.toml"))
    );
    assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_STARRY_ARCH));
    assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_STARRY_TARGET));
    assert_eq!(
        snapshot.config,
        Some(PathBuf::from(
            "tmp/axbuild/config/starryos/build-riscv64gc-unknown-none-elf.toml"
        ))
    );
}

#[test]
fn prepare_starry_request_inherits_snapshot_config() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    write_snapshot_text(
        root.path(),
        STARRY_SNAPSHOT_FILE,
        r#"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
config = "configs/custom-starry.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) =
        prepare_starry_request(&app, StarryCliArgs::default(), None, None).unwrap();

    assert_eq!(request.arch, "aarch64");
    assert_eq!(request.target, "aarch64-unknown-none-softfloat");
    assert_eq!(
        request.build_info_path,
        root.path().join("configs/custom-starry.toml")
    );
    assert_eq!(
        snapshot.config,
        Some(PathBuf::from("configs/custom-starry.toml"))
    );
}

#[test]
fn prepare_starry_request_explicit_config_target_overrides_snapshot_target() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    let config = root.path().join("configs/sg2002.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        r#"
target = "riscv64gc-unknown-none-elf"
features = [
    "plat-dyn",
    "starry-kernel/sg2002",
    "axplat-dyn/thead-mae",
]
log = "Info"
"#,
    )
    .unwrap();
    write_snapshot_text(
        root.path(),
        STARRY_SNAPSHOT_FILE,
        r#"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_starry_request(
        &app,
        StarryCliArgs {
            config: Some(config.clone()),
            arch: None,
            target: None,
            smp: None,
            debug: false,
        },
        None,
        None,
    )
    .unwrap();

    assert_eq!(request.arch, "riscv64");
    assert_eq!(request.target, "riscv64gc-unknown-none-elf");
    assert_eq!(request.build_info_path, config);
    assert_eq!(snapshot.arch.as_deref(), Some("riscv64"));
    assert_eq!(
        snapshot.target.as_deref(),
        Some("riscv64gc-unknown-none-elf")
    );
}

#[test]
fn prepare_starry_request_rejects_mismatched_arch_and_target() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    let app = test_app_context(root.path());

    let err = prepare_starry_request(
        &app,
        StarryCliArgs {
            config: None,
            arch: Some("aarch64".into()),
            target: Some("x86_64-unknown-none".into()),
            smp: None,
            debug: false,
        },
        None,
        None,
    )
    .unwrap_err();

    assert!(err.to_string().contains("maps to target"));
}

#[test]
fn prepare_starry_request_cli_arch_overrides_snapshot_target() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    write_snapshot_text(
        root.path(),
        STARRY_SNAPSHOT_FILE,
        r#"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_starry_request(
        &app,
        StarryCliArgs {
            config: None,
            arch: Some("riscv64".into()),
            target: None,
            smp: None,
            debug: false,
        },
        None,
        None,
    )
    .unwrap();

    assert_eq!(request.arch, "riscv64");
    assert_eq!(request.target, "riscv64gc-unknown-none-elf");
    assert_eq!(snapshot.arch.as_deref(), Some("riscv64"));
    assert_eq!(
        snapshot.target.as_deref(),
        Some("riscv64gc-unknown-none-elf")
    );
}

#[test]
fn prepare_starry_request_cli_target_overrides_snapshot_arch() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    write_snapshot_text(
        root.path(),
        STARRY_SNAPSHOT_FILE,
        r#"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_starry_request(
        &app,
        StarryCliArgs {
            config: None,
            arch: None,
            target: Some("x86_64-unknown-none".into()),
            smp: None,
            debug: false,
        },
        None,
        None,
    )
    .unwrap();

    assert_eq!(request.arch, "x86_64");
    assert_eq!(request.target, "x86_64-unknown-none");
    assert_eq!(snapshot.arch.as_deref(), Some("x86_64"));
    assert_eq!(snapshot.target.as_deref(), Some("x86_64-unknown-none"));
}

#[test]
fn prepare_starry_request_cli_arch_drops_stale_snapshot_runtime_paths() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    write_snapshot_text(
        root.path(),
        STARRY_SNAPSHOT_FILE,
        r#"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"

[qemu]
qemu_config = "os/StarryOS/starryos/.qemu-aarch64.toml"

[uboot]
uboot_config = "configs/uboot-aarch64.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_starry_request(
        &app,
        StarryCliArgs {
            config: None,
            arch: Some("riscv64".into()),
            target: None,
            smp: None,
            debug: false,
        },
        None,
        None,
    )
    .unwrap();

    assert_eq!(request.arch, "riscv64");
    assert_eq!(request.target, "riscv64gc-unknown-none-elf");
    assert_eq!(request.qemu_config, None);
    assert_eq!(request.uboot_config, None);
    assert_eq!(snapshot.qemu.qemu_config, None);
    assert_eq!(snapshot.uboot.uboot_config, None);
}

#[test]
fn starry_arch_target_mapping_helpers_work() {
    assert_eq!(
        starry_target_for_arch_checked(DEFAULT_STARRY_ARCH).unwrap(),
        DEFAULT_STARRY_TARGET
    );
    assert_eq!(
        starry_arch_for_target_checked("x86_64-unknown-none").unwrap(),
        "x86_64"
    );
    assert!(starry_target_for_arch_checked("mips64").is_err());
    assert!(starry_arch_for_target_checked("mips64-unknown-none").is_err());
}

#[test]
fn resolve_starry_arch_and_target_infers_arch_from_target() {
    let (arch, target) =
        resolve_starry_arch_and_target(None, Some("x86_64-unknown-none".into())).unwrap();

    assert_eq!(arch, "x86_64");
    assert_eq!(target, "x86_64-unknown-none");
}
