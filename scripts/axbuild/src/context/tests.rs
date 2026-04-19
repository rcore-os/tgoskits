use std::{
    fs,
    path::{Path, PathBuf},
};

use ostool::{Tool, ToolConfig};
use tempfile::tempdir;

use super::*;

fn test_app_context(root: &Path) -> AppContext {
    AppContext {
        tool: Tool::new(ToolConfig::default()).unwrap(),
        build_config_path: None,
        root: root.to_path_buf(),
        axvisor_dir: Some(root.join("os/axvisor")),
        debug: false,
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

#[test]
fn snapshot_load_returns_default_when_missing() {
    let root = tempdir().unwrap();
    let snapshot = ArceosCommandSnapshot::load(root.path()).unwrap();
    assert_eq!(snapshot, ArceosCommandSnapshot::default());
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
        package: Some("ax-helloworld".into()),
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

    assert_eq!(path, root.path().join(ARCEOS_SNAPSHOT_FILE));
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
        config: Some(PathBuf::from("os/axvisor/.build.toml")),
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

    assert_eq!(path, root.path().join(AXVISOR_SNAPSHOT_FILE));
    assert_eq!(loaded, snapshot);
}

#[test]
fn prepare_request_prefers_cli_over_snapshot() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join(ARCEOS_SNAPSHOT_FILE),
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

    let (request, snapshot) = app
        .prepare_arceos_request(
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
    fs::write(
        root.path().join(ARCEOS_SNAPSHOT_FILE),
        r#"
package = "ax-helloworld"

[qemu]
qemu_config = "configs/qemu.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = app
        .prepare_arceos_request(BuildCliArgs::default(), None, None)
        .unwrap();

    assert_eq!(request.package, "ax-helloworld");
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
fn prepare_request_requires_package() {
    let root = tempdir().unwrap();
    let app = test_app_context(root.path());

    let err = app
        .prepare_arceos_request(BuildCliArgs::default(), None, None)
        .unwrap_err();

    assert!(err.to_string().contains("missing ArceOS package"));
}

#[test]
fn prepare_request_resolves_arceos_target_from_arch() {
    let root = tempdir().unwrap();
    let app = test_app_context(root.path());

    let (request, snapshot) = app
        .prepare_arceos_request(
            BuildCliArgs {
                config: None,
                package: Some("ax-helloworld".into()),
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
fn prepare_request_cli_target_drops_stale_arceos_runtime_paths() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join(ARCEOS_SNAPSHOT_FILE),
        r#"
package = "ax-helloworld"
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

    let (request, snapshot) = app
        .prepare_arceos_request(
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
    fs::write(
        root.path().join(AXVISOR_SNAPSHOT_FILE),
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

    let mut app = test_app_context(root.path());

    let (request, snapshot) = app
        .prepare_axvisor_request(
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
    fs::write(
        root.path().join(AXVISOR_SNAPSHOT_FILE),
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

    let mut app = test_app_context(root.path());

    let (request, snapshot) = app
        .prepare_axvisor_request(AxvisorCliArgs::default(), None, None)
        .unwrap();

    assert_eq!(request.arch, DEFAULT_AXVISOR_ARCH);
    assert_eq!(request.target, DEFAULT_AXVISOR_TARGET);
    assert_eq!(request.plat_dyn, None);
    assert_eq!(
        request.build_info_path,
        root.path()
            .join("os/axvisor/.build-aarch64-unknown-none-softfloat.toml")
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
            "os/axvisor/.build-aarch64-unknown-none-softfloat.toml"
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
    let mut app = test_app_context(root.path());

    let (request, snapshot) = app
        .prepare_axvisor_request(
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
            .join("os/axvisor/.build-x86_64-unknown-none.toml")
    );
    assert_eq!(snapshot.arch.as_deref(), Some("x86_64"));
    assert_eq!(snapshot.target.as_deref(), Some("x86_64-unknown-none"));
}

#[test]
fn prepare_axvisor_request_cli_arch_drops_stale_runtime_paths() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join(AXVISOR_SNAPSHOT_FILE),
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

    let mut app = test_app_context(root.path());

    let (request, snapshot) = app
        .prepare_axvisor_request(
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
fn prepare_axvisor_request_rewrites_stale_generated_snapshot_config_path() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join(AXVISOR_SNAPSHOT_FILE),
        r#"
config = "os/axvisor/.build-riscv64gc-unknown-none-elf.toml"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();

    let mut app = test_app_context(root.path());

    let (request, snapshot) = app
        .prepare_axvisor_request(AxvisorCliArgs::default(), None, None)
        .unwrap();

    assert_eq!(
        request.build_info_path,
        root.path()
            .join("os/axvisor/.build-aarch64-unknown-none-softfloat.toml")
    );
    assert_eq!(
        snapshot.config,
        Some(PathBuf::from(
            "os/axvisor/.build-aarch64-unknown-none-softfloat.toml"
        ))
    );
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
        qemu: StarryQemuSnapshot {
            qemu_config: Some(PathBuf::from("configs/qemu.toml")),
        },
        uboot: StarryUbootSnapshot {
            uboot_config: Some(PathBuf::from("configs/uboot.toml")),
        },
    };

    let path = snapshot.store(root.path()).unwrap();
    let loaded = StarryCommandSnapshot::load(root.path()).unwrap();

    assert_eq!(path, root.path().join(STARRY_SNAPSHOT_FILE));
    assert_eq!(loaded, snapshot);
}

#[test]
fn prepare_starry_request_prefers_cli_over_snapshot() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    fs::write(
        root.path().join(STARRY_SNAPSHOT_FILE),
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

    let (request, snapshot) = app
        .prepare_starry_request(
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
        snapshot.qemu.qemu_config,
        Some(PathBuf::from("/tmp/qemu.toml"))
    );
    assert_eq!(snapshot.uboot.uboot_config, None);
}

#[test]
fn prepare_starry_request_uses_snapshot_and_default_arch() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    fs::write(
        root.path().join(STARRY_SNAPSHOT_FILE),
        r#"
[qemu]
qemu_config = "configs/qemu.toml"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = app
        .prepare_starry_request(StarryCliArgs::default(), None, None)
        .unwrap();

    assert_eq!(request.package, STARRY_PACKAGE);
    assert_eq!(request.arch, DEFAULT_STARRY_ARCH);
    assert_eq!(request.target, DEFAULT_STARRY_TARGET);
    assert_eq!(request.plat_dyn, None);
    assert_eq!(
        request.build_info_path,
        root.path()
            .join("os/StarryOS/starryos/.build-riscv64gc-unknown-none-elf.toml")
    );
    assert_eq!(
        request.qemu_config,
        Some(root.path().join("configs/qemu.toml"))
    );
    assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_STARRY_ARCH));
    assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_STARRY_TARGET));
}

#[test]
fn prepare_starry_request_rejects_mismatched_arch_and_target() {
    let root = tempdir().unwrap();
    prepare_starry_workspace(root.path());
    let app = test_app_context(root.path());

    let err = app
        .prepare_starry_request(
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
    fs::write(
        root.path().join(STARRY_SNAPSHOT_FILE),
        r#"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = app
        .prepare_starry_request(
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
    fs::write(
        root.path().join(STARRY_SNAPSHOT_FILE),
        r#"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();

    let app = test_app_context(root.path());

    let (request, snapshot) = app
        .prepare_starry_request(
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
    fs::write(
        root.path().join(STARRY_SNAPSHOT_FILE),
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

    let (request, snapshot) = app
        .prepare_starry_request(
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
