use super::{common::*, *};

#[test]
fn prepare_request_prefers_cli_over_snapshot() {
    let root = tempdir().unwrap();
    write_snapshot_text(
        root.path(),
        ARCEOS_SNAPSHOT_FILE,
        r#"
package = "from-snapshot"
arch = "riscv64"
target = "snapshot-target"
plat_dyn = false

[qemu]
qemu_config = "configs/snapshot-qemu.toml"

[uboot]
uboot_config = "configs/snapshot-uboot.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_arceos_request(
        &app,
        BuildCliArgs {
            config: Some(PathBuf::from("/tmp/custom-build.toml")),
            package: Some("from-cli".into()),
            arch: Some("aarch64".into()),
            target: Some(DEFAULT_ARCEOS_TARGET.into()),
            plat_dyn: Some(true),
            smp: Some(4),
            debug: true,
        },
        Some(PathBuf::from("/tmp/qemu.toml")),
        None,
    )
    .unwrap();

    assert_eq!(request.package, "from-cli");
    assert_eq!(request.target, DEFAULT_ARCEOS_TARGET);
    assert_eq!(request.plat_dyn, Some(true));
    assert_eq!(request.smp, Some(4));
    assert!(request.debug);
    assert_eq!(
        request.build_info_path,
        PathBuf::from("/tmp/custom-build.toml")
    );
    assert_eq!(request.qemu_config, Some(PathBuf::from("/tmp/qemu.toml")));
    assert_eq!(request.uboot_config, None);
    assert_eq!(snapshot.package.as_deref(), Some("from-cli"));
    assert_eq!(snapshot.arch.as_deref(), Some("aarch64"));
    assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_ARCEOS_TARGET));
    assert_eq!(snapshot.plat_dyn, Some(true));
    assert_eq!(snapshot.smp, Some(4));
    assert_eq!(
        snapshot.qemu.qemu_config,
        Some(PathBuf::from("/tmp/qemu.toml"))
    );
    assert_eq!(snapshot.uboot.uboot_config, None);
}

#[test]
fn prepare_request_uses_snapshot_and_default_target() {
    let root = tempdir().unwrap();
    write_snapshot_text(
        root.path(),
        ARCEOS_SNAPSHOT_FILE,
        r#"
package = "arceos-helloworld"

[qemu]
qemu_config = "configs/qemu.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) =
        prepare_arceos_request(&app, BuildCliArgs::default(), None, None).unwrap();

    assert_eq!(request.package, "arceos-helloworld");
    assert_eq!(request.arch, DEFAULT_ARCEOS_ARCH);
    assert_eq!(request.target, DEFAULT_ARCEOS_TARGET);
    assert_eq!(request.plat_dyn, None);
    assert_eq!(
        request.qemu_config,
        Some(root.path().join("configs/qemu.toml"))
    );
    assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_ARCEOS_ARCH));
    assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_ARCEOS_TARGET));
}

#[test]
fn prepare_request_explicit_config_drops_snapshot_plat_dyn() {
    let root = tempdir().unwrap();
    write_snapshot_text(
        root.path(),
        ARCEOS_SNAPSHOT_FILE,
        r#"
package = "from-snapshot"
arch = "riscv64"
target = "riscv64gc-unknown-none-elf"
plat_dyn = false
smp = 4

[qemu]
qemu_config = "configs/snapshot-qemu.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_arceos_request(
        &app,
        BuildCliArgs {
            config: Some(PathBuf::from("/tmp/build-riscv64.toml")),
            package: Some("arceos-test-suit".into()),
            arch: None,
            target: Some("riscv64gc-unknown-none-elf".into()),
            plat_dyn: None,
            smp: None,
            debug: false,
        },
        None,
        None,
    )
    .unwrap();

    assert_eq!(request.package, "arceos-test-suit");
    assert_eq!(request.plat_dyn, None);
    assert_eq!(request.smp, None);
    assert_eq!(request.qemu_config, None);
    assert_eq!(snapshot.plat_dyn, None);
    assert_eq!(snapshot.smp, None);
    assert_eq!(snapshot.qemu.qemu_config, None);
}

#[test]
fn prepare_request_requires_package() {
    let root = tempdir().unwrap();
    let app = test_app_context(root.path());

    let err = prepare_arceos_request(&app, BuildCliArgs::default(), None, None).unwrap_err();

    assert!(err.to_string().contains("missing ArceOS package"));
}

#[test]
fn prepare_request_resolves_arceos_target_from_arch() {
    let root = tempdir().unwrap();
    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_arceos_request(
        &app,
        BuildCliArgs {
            config: None,
            package: Some("arceos-helloworld".into()),
            arch: Some("x86_64".into()),
            target: None,
            plat_dyn: None,
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
fn should_use_loongarch_lvz_only_for_axvisor_loongarch() {
    assert!(should_use_loongarch_lvz_for(
        crate::axvisor::build::AXVISOR_PACKAGE,
        "loongarch64-unknown-none-softfloat"
    ));
    assert!(!should_use_loongarch_lvz_for(
        crate::axvisor::build::AXVISOR_PACKAGE,
        "riscv64gc-unknown-none-elf"
    ));
    assert!(!should_use_loongarch_lvz_for(
        STARRY_PACKAGE,
        "loongarch64-unknown-none-softfloat"
    ));
}

#[test]
fn find_loongarch_qemu_dir_prefers_explicit_env_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    let root = tempdir().unwrap();
    let qemu_bin_dir = tempdir().unwrap();
    let fallback_dir = tempdir().unwrap();
    fs::write(qemu_bin_dir.path().join("qemu-system-loongarch64"), "").unwrap();
    fs::write(fallback_dir.path().join("qemu-system-loongarch64"), "").unwrap();

    let _qemu_dir = TempEnvVar::set("AXBUILD_QEMU_DIR", fallback_dir.path());
    let _qemu_bin = TempEnvVar::set(
        "AXBUILD_QEMU_SYSTEM_LOONGARCH64",
        qemu_bin_dir.path().join("qemu-system-loongarch64"),
    );
    let _home = TempEnvVar::unset("HOME");

    assert_eq!(
        find_loongarch_qemu_dir(root.path()),
        Some(qemu_bin_dir.path().to_path_buf())
    );
}

#[test]
fn find_loongarch_qemu_dir_uses_latest_cache() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let qemu_dir = home
        .path()
        .join(".cache/axvisor/qemu-lvz")
        .join("latest")
        .join("bin");

    fs::create_dir_all(&qemu_dir).unwrap();
    fs::write(qemu_dir.join("qemu-system-loongarch64"), "").unwrap();

    let _qemu_dir = TempEnvVar::unset("AXBUILD_QEMU_DIR");
    let _qemu_bin = TempEnvVar::unset("AXBUILD_QEMU_SYSTEM_LOONGARCH64");
    let _home = TempEnvVar::set("HOME", home.path());

    assert_eq!(find_loongarch_qemu_dir(workspace.path()), Some(qemu_dir));
}

#[test]
fn find_loongarch_qemu_dir_honors_custom_latest_cache_root() {
    let _lock = ENV_LOCK.lock().unwrap();
    let cache = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let qemu_dir = cache.path().join("latest").join("bin");

    fs::create_dir_all(&qemu_dir).unwrap();
    fs::write(qemu_dir.join("qemu-system-loongarch64"), "").unwrap();

    let _qemu_dir = TempEnvVar::unset("AXBUILD_QEMU_DIR");
    let _qemu_bin = TempEnvVar::unset("AXBUILD_QEMU_SYSTEM_LOONGARCH64");
    let _home = TempEnvVar::unset("HOME");
    let _cache = TempEnvVar::set("AXVISOR_QEMU_LVZ_CACHE", cache.path());

    assert_eq!(find_loongarch_qemu_dir(workspace.path()), Some(qemu_dir));
}

#[test]
fn find_loongarch_qemu_dir_uses_existing_cached_version() {
    let _lock = ENV_LOCK.lock().unwrap();
    let cache = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let qemu_dir = cache.path().join("abcdef1234567890").join("bin");

    fs::create_dir_all(&qemu_dir).unwrap();
    fs::write(qemu_dir.join("qemu-system-loongarch64"), "").unwrap();

    let _qemu_dir = TempEnvVar::unset("AXBUILD_QEMU_DIR");
    let _qemu_bin = TempEnvVar::unset("AXBUILD_QEMU_SYSTEM_LOONGARCH64");
    let _home = TempEnvVar::unset("HOME");
    let _cache = TempEnvVar::set("AXVISOR_QEMU_LVZ_CACHE", cache.path());

    assert_eq!(find_loongarch_qemu_dir(workspace.path()), Some(qemu_dir));
}

#[test]
fn prepare_request_cli_target_drops_stale_arceos_runtime_paths() {
    let root = tempdir().unwrap();
    write_snapshot_text(
        root.path(),
        ARCEOS_SNAPSHOT_FILE,
        r#"
package = "arceos-helloworld"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"

[qemu]
qemu_config = "configs/qemu-aarch64.toml"

[uboot]
uboot_config = "configs/uboot-aarch64.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = prepare_arceos_request(
        &app,
        BuildCliArgs {
            config: None,
            package: None,
            arch: None,
            target: Some("riscv64gc-unknown-none-elf".into()),
            plat_dyn: None,
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
