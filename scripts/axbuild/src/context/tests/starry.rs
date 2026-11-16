use super::{common::*, *};

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

    let custom_config = root.path().join("starry-build.toml");
    fs::write(&custom_config, "").unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_starry_request(
        &app,
        StarryCliArgs {
            preset: None,
            config: Some(custom_config.clone()),
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
    assert_eq!(request.config_path().unwrap().to_path_buf(), custom_config);
    assert_eq!(request.qemu_config, Some(PathBuf::from("/tmp/qemu.toml")));
    assert_eq!(request.uboot_config, None);
    assert_eq!(snapshot.arch.as_deref(), Some("aarch64"));
    assert_eq!(
        snapshot.target.as_deref(),
        Some("aarch64-unknown-none-softfloat")
    );
    assert_eq!(snapshot.smp, Some(4));
    assert_eq!(snapshot.config, Some(PathBuf::from("starry-build.toml")));
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
    assert_eq!(request.config_path(), None);
    assert_eq!(request.source.preset(), snapshot.preset.as_deref());
    assert!(snapshot.preset.is_some());
    assert!(!root.path().join("tmp/axbuild/config").exists());
    assert_eq!(
        request.qemu_config,
        Some(root.path().join("configs/qemu.toml"))
    );
    assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_STARRY_ARCH));
    assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_STARRY_TARGET));
    assert_eq!(snapshot.config, None);
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

    let saved_path = snapshot_path(root.path(), STARRY_SNAPSHOT_FILE);
    let saved = fs::read(&saved_path).unwrap();
    let missing = prepare_starry_request(&app, StarryCliArgs::default(), None, None).unwrap_err();
    assert!(format!("{missing:#}").contains("--preset"));
    assert_eq!(fs::read(&saved_path).unwrap(), saved);
    let custom = root.path().join("configs/custom-starry.toml");
    fs::create_dir_all(custom.parent().unwrap()).unwrap();
    fs::write(&custom, "").unwrap();

    let (request, snapshot) =
        prepare_starry_request(&app, StarryCliArgs::default(), None, None).unwrap();

    assert_eq!(request.arch, "aarch64");
    assert_eq!(request.target, "aarch64-unknown-none-softfloat");
    assert_eq!(
        request.config_path().unwrap().to_path_buf(),
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
            preset: None,
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
    assert_eq!(request.config_path().unwrap().to_path_buf(), config);
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
            preset: None,
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
            preset: None,
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
fn prepare_starry_request_cli_arch_drops_stale_snapshot_runtime_paths() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    write_snapshot_text(
        root.path(),
        STARRY_SNAPSHOT_FILE,
        r#"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
smp = 1

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
            preset: None,
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
    assert_eq!(request.smp, None);
    assert_eq!(request.qemu_config, None);
    assert_eq!(request.uboot_config, None);
    assert_eq!(snapshot.smp, None);
    assert_eq!(snapshot.qemu.qemu_config, None);
    assert_eq!(snapshot.uboot.uboot_config, None);
}
