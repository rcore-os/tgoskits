use std::{
    collections::HashMap,
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use ostool::invocation::{Invocation, InvocationOptions};
use tempfile::tempdir;

use super::*;

fn test_app_context(root: &Path) -> AppContext {
    AppContext {
        invocation: test_invocation(root),
        build_config_path: None,
        root: root.to_path_buf(),
        member_dirs: HashMap::from([("axvisor".to_string(), root.join("os/axvisor"))]),
        original_path: env::var_os("PATH").unwrap_or_default(),
        debug: false,
    }
}

fn test_invocation(root: &Path) -> Invocation {
    let manifest_path = root.join("Cargo.toml");
    if !manifest_path.exists() {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(
            &manifest_path,
            r#"[package]
name = "test-workspace"
version = "0.1.0"
edition = "2021"

[workspace]
"#,
        )
        .unwrap();
    }
    Invocation::new(InvocationOptions::new(
        Some(manifest_path),
        None,
        None,
        false,
    ))
    .unwrap()
}

fn resolve_arceos_build_info_path(
    package: &str,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    crate::arceos::build::resolve_build_info_path(package, target, explicit_path)
}

fn prepare_arceos_request(
    app: &AppContext,
    cli: BuildCliArgs,
    qemu_config: Option<PathBuf>,
    uboot_config: Option<PathBuf>,
) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
    app.prepare_arceos_request(
        cli,
        qemu_config,
        uboot_config,
        resolve_arceos_build_info_path,
    )
}

fn resolve_starry_build_info_path(
    workspace_root: &Path,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    crate::starry::build::resolve_build_info_path(workspace_root, target, explicit_path)
}

fn prepare_starry_request(
    app: &AppContext,
    cli: StarryCliArgs,
    qemu_config: Option<PathBuf>,
    uboot_config: Option<PathBuf>,
) -> anyhow::Result<(ResolvedStarryRequest, StarryCommandSnapshot)> {
    app.prepare_starry_request(
        cli,
        qemu_config,
        uboot_config,
        resolve_starry_build_info_path,
    )
}

fn prepare_axvisor_request(
    app: &AppContext,
    cli: AxvisorCliArgs,
    qemu_config: Option<PathBuf>,
    uboot_config: Option<PathBuf>,
) -> anyhow::Result<(ResolvedAxvisorRequest, AxvisorCommandSnapshot)> {
    app.prepare_axvisor_request(
        cli,
        AxvisorRequestPaths {
            package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
            axvisor_dir: app.root.join("os/axvisor"),
            load_config_target: crate::axvisor::build::load_target_from_build_config,
            resolve_build_info_path: crate::axvisor::build::resolve_build_info_path,
        },
        qemu_config,
        uboot_config,
    )
}

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct TempEnvVar {
    key: &'static str,
    original: Option<OsString>,
}

impl TempEnvVar {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::set_var(key, value);
        }
        Self { key, original }
    }

    fn unset(key: &'static str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for TempEnvVar {
    fn drop(&mut self) {
        match self.original.as_ref() {
            Some(value) => unsafe {
                env::set_var(self.key, value);
            },
            None => unsafe {
                env::remove_var(self.key);
            },
        }
    }
}

fn write_minimal_workspace_package(path: &Path, name: &str) {
    let src_dir = path.parent().unwrap().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("lib.rs"), "").unwrap();
    fs::write(
        path,
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .unwrap();
}

fn prepare_starry_workspace(root: &Path) {
    let starry_dir = root.join("os/StarryOS/starryos");
    fs::create_dir_all(&starry_dir).unwrap();
    write_minimal_workspace_package(&starry_dir.join("Cargo.toml"), STARRY_PACKAGE);
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
    )
    .unwrap();
}

fn snapshot_path(root: &Path, file_name: &str) -> PathBuf {
    axbuild_tmp_dir(root).join(file_name)
}

fn write_snapshot_text(root: &Path, file_name: &str, content: &str) -> std::io::Result<()> {
    let path = snapshot_path(root, file_name);
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, content)
}

#[test]
fn snapshot_load_returns_default_when_missing() {
    let root = tempdir().unwrap();
    let snapshot = ArceosCommandSnapshot::load(root.path()).unwrap();
    assert_eq!(snapshot, ArceosCommandSnapshot::default());
}

#[test]
fn snapshot_persistence_can_be_disabled_by_env() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _env = TempEnvVar::set(NO_SNAPSHOT_ENV, "1");

    assert!(!SnapshotPersistence::Store.should_store());
    assert!(!SnapshotPersistence::Discard.should_store());
}

#[test]
fn snapshot_persistence_treats_zero_env_as_enabled() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _env = TempEnvVar::set(NO_SNAPSHOT_ENV, "0");

    assert!(SnapshotPersistence::Store.should_store());
    assert!(!SnapshotPersistence::Discard.should_store());
}

#[test]
fn axvisor_snapshot_load_returns_default_when_missing() {
    let root = tempdir().unwrap();
    let snapshot = AxvisorCommandSnapshot::load(root.path()).unwrap();
    assert_eq!(snapshot, AxvisorCommandSnapshot::default());
}

#[test]
fn snapshot_store_round_trips() {
    let root = tempdir().unwrap();
    let snapshot = ArceosCommandSnapshot {
        package: Some("arceos-helloworld".into()),
        arch: Some("aarch64".into()),
        target: Some("target".into()),
        plat_dyn: Some(true),
        smp: None,
        qemu: ArceosQemuSnapshot {
            qemu_config: Some(PathBuf::from("configs/qemu.toml")),
        },
        uboot: ArceosUbootSnapshot {
            uboot_config: Some(PathBuf::from("configs/uboot.toml")),
        },
    };

    let path = snapshot.store(root.path()).unwrap();
    let loaded = ArceosCommandSnapshot::load(root.path()).unwrap();

    assert_eq!(path, snapshot_path(root.path(), ARCEOS_SNAPSHOT_FILE));
    assert_eq!(loaded, snapshot);
}

#[test]
fn axvisor_snapshot_store_round_trips() {
    let root = tempdir().unwrap();
    let snapshot = AxvisorCommandSnapshot {
        arch: Some("aarch64".into()),
        target: Some(DEFAULT_AXVISOR_TARGET.into()),
        plat_dyn: Some(false),
        smp: None,
        config: Some(PathBuf::from(
            "tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml",
        )),
        vmconfigs: vec![PathBuf::from("tmp/vm1.toml"), PathBuf::from("tmp/vm2.toml")],
        qemu: AxvisorQemuSnapshot {
            qemu_config: Some(PathBuf::from("configs/qemu.toml")),
        },
        uboot: AxvisorUbootSnapshot {
            uboot_config: Some(PathBuf::from("configs/uboot.toml")),
        },
    };

    let path = snapshot.store(root.path()).unwrap();
    let loaded = AxvisorCommandSnapshot::load(root.path()).unwrap();

    assert_eq!(path, snapshot_path(root.path(), AXVISOR_SNAPSHOT_FILE));
    assert_eq!(loaded, snapshot);
}

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
fn find_loongarch_qemu_dir_checks_home_before_workspace_ancestors() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let workspace_parent = tempdir().unwrap();
    let workspace_root = workspace_parent.path().join("workspace/repo");
    let home_qemu_dir = home.path().join("QEMU-LVZ/build");
    let ancestor_qemu_dir = workspace_parent.path().join("qemu-lvz/build");

    fs::create_dir_all(&workspace_root).unwrap();
    fs::create_dir_all(&home_qemu_dir).unwrap();
    fs::create_dir_all(&ancestor_qemu_dir).unwrap();
    fs::write(home_qemu_dir.join("qemu-system-loongarch64"), "").unwrap();
    fs::write(ancestor_qemu_dir.join("qemu-system-loongarch64"), "").unwrap();

    let _qemu_dir = TempEnvVar::unset("AXBUILD_QEMU_DIR");
    let _qemu_bin = TempEnvVar::unset("AXBUILD_QEMU_SYSTEM_LOONGARCH64");
    let _home = TempEnvVar::set("HOME", home.path());

    assert_eq!(
        find_loongarch_qemu_dir(&workspace_root),
        Some(home_qemu_dir)
    );
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
    assert_eq!(request.plat_dyn, None);
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
    assert_eq!(request.plat_dyn, None);
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
features = ["sg2002"]
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
