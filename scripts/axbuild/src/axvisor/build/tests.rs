use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use tempfile::tempdir;

use super::*;

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

fn write_board(axvisor_dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = axvisor_dir
        .join("configs/board")
        .join(format!("{name}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    path
}

fn request(path: PathBuf, arch: &str, target: &str) -> ResolvedAxvisorRequest {
    ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("os/axvisor")),
        arch: arch.to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        build_info_path: path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    }
}

#[test]
fn resolve_build_info_path_uses_default_axvisor_location() {
    let root = tempdir().unwrap();
    let path = resolve_build_info_path(
        &root.path().join("os/axvisor"),
        "aarch64-unknown-none-softfloat",
        None,
    )
    .unwrap();

    assert_eq!(
        path,
        root.path()
            .join("tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml")
    );
}

#[test]
fn resolve_build_info_path_prefers_explicit_path() {
    let root = tempdir().unwrap();
    let explicit = root.path().join("custom/build.toml");
    let path = resolve_build_info_path(
        &root.path().join("os/axvisor"),
        "x86_64-unknown-none",
        Some(explicit.clone()),
    )
    .unwrap();

    assert_eq!(path, explicit);
}

#[test]
fn resolve_build_info_path_ignores_source_tree_defaults() {
    let root = tempdir().unwrap();
    let axvisor_dir = root.path().join("os/axvisor");
    fs::create_dir_all(&axvisor_dir).unwrap();
    let bare = axvisor_dir.join("build-aarch64-unknown-none-softfloat.toml");
    let dotted = axvisor_dir.join(".build-aarch64-unknown-none-softfloat.toml");
    fs::write(&bare, "").unwrap();
    fs::write(&dotted, "").unwrap();

    let path =
        resolve_build_info_path(&axvisor_dir, "aarch64-unknown-none-softfloat", None).unwrap();

    assert_eq!(
        path,
        root.path()
            .join("tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml")
    );
}

#[test]
fn load_cargo_config_writes_default_template_when_missing() {
    let root = tempdir().unwrap();
    let path = root
        .path()
        .join("os/axvisor/.build-aarch64-unknown-none-softfloat.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_board(
        path.parent().unwrap(),
        "qemu-aarch64",
        r#"
target = "aarch64-unknown-none-softfloat"
features = ["ept-level-4"]
log = "Info"
vm_configs = []
"#,
    );

    let cargo = load_cargo_config(&request(
        path.clone(),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    ))
    .unwrap();

    assert!(cargo.features.contains(&"ept-level-4".to_string()));
    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-driver/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"axvm/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"dyn-plat".to_string()));
    assert!(path.exists());
}

#[test]
fn load_cargo_config_injects_vmconfigs() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    let vmconfigs = vec![root.path().join("a.toml"), root.path().join("b.toml")];
    fs::write(
        &config_path,
        r#"
features = ["fs", "ept-level-4"]
log = "Info"
"#,
    )
    .unwrap();

    let cargo = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "aarch64".to_string(),
        target: "aarch64-unknown-none-softfloat".to_string(),
        smp: None,
        debug: false,
        build_info_path: config_path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vmconfigs.clone(),
    })
    .unwrap();

    assert_eq!(cargo.package, AXVISOR_PACKAGE);
    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json")
    );
    assert_eq!(
        cargo.env.get("AX_ARCH").map(String::as_str),
        Some("aarch64")
    );
    assert_eq!(
        cargo.env.get("AX_TARGET").map(String::as_str),
        Some("aarch64-unknown-none-softfloat")
    );
    assert_eq!(
        cargo.env.get("AXVISOR_VM_CONFIGS").map(String::as_str),
        Some(
            std::env::join_paths(&vmconfigs)
                .unwrap()
                .to_string_lossy()
                .as_ref()
        )
    );
    assert_eq!(
        cargo
            .args
            .windows(2)
            .find_map(|window| (window[0] == "--bin").then_some(window[1].as_str())),
        Some("axvisor")
    );
}

#[test]
fn load_target_from_board_config_reads_target() {
    let root = tempdir().unwrap();
    let path = root.path().join("qemu-aarch64.toml");
    fs::write(
        &path,
        r#"
features = []
log = "Info"
target = "aarch64-unknown-none-softfloat"
vm_configs = []
"#,
    )
    .unwrap();

    assert_eq!(
        load_target_from_build_config(&path).unwrap(),
        Some("aarch64-unknown-none-softfloat".to_string())
    );
}

#[test]
fn load_target_from_plain_build_config_returns_none() {
    let root = tempdir().unwrap();
    let path = root.path().join(".build.toml");
    fs::write(
        &path,
        r#"
features = ["fs"]
log = "Info"
"#,
    )
    .unwrap();

    assert_eq!(load_target_from_build_config(&path).unwrap(), None);
}

#[test]
fn load_target_from_build_config_rejects_removed_std_field() {
    let root = tempdir().unwrap();
    let path = root.path().join("qemu-aarch64.toml");
    fs::write(
        &path,
        r#"
std = true
features = []
log = "Info"
target = "aarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();

    let err = load_target_from_build_config(&path).unwrap_err();

    assert!(
        err.to_string().contains("uses removed `std` field"),
        "{err:#}"
    );
}

#[test]
fn load_target_from_build_config_rejects_arceos_app_c_field() {
    let root = tempdir().unwrap();
    let path = root.path().join("qemu-aarch64.toml");
    fs::write(
        &path,
        r#"
app-c = "c"
features = []
log = "Info"
target = "aarch64-unknown-none-softfloat"
"#,
    )
    .unwrap();

    let err = load_target_from_build_config(&path).unwrap_err();

    assert!(
        err.to_string().contains("uses ArceOS-only `app-c` field"),
        "{err:#}"
    );
}

#[test]
fn load_cargo_config_uses_board_defaults_when_default_file_is_missing() {
    let root = tempdir().unwrap();
    let path = root
        .path()
        .join("os/axvisor/.build-x86_64-unknown-none.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let board_path = write_board(
        path.parent().unwrap(),
        "qemu-x86_64",
        r#"
target = "x86_64-unknown-none"
features = ["ept-level-4", "fs", "vmx"]
log = "Info"
vm_configs = []
"#,
    );

    let cargo = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "x86_64".to_string(),
        target: "x86_64-unknown-none".to_string(),
        smp: None,
        debug: false,
        build_info_path: path.clone(),
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    })
    .unwrap();

    assert!(path.exists());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        fs::read_to_string(board_path).unwrap()
    );
    assert!(cargo.features.contains(&"ept-level-4".to_string()));
    assert!(cargo.features.contains(&"fs".to_string()));
    assert!(cargo.features.contains(&"vmx".to_string()));
    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"axvm/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-driver/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/defplat".to_string()));
    assert!(!cargo.features.contains(&"ax-std/x86-pc".to_string()));
    assert!(!cargo.features.contains(&"ax-hal/x86-pc".to_string()));
    assert!(!cargo.features.contains(&"ax-hal/x86-qemu-q35".to_string()));
}

#[test]
fn load_cargo_config_defaults_aarch64_to_dynamic_platform() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["ax-std", "ept-level-4"]
log = "Info"
"#,
    )
    .unwrap();

    let cargo = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "aarch64".to_string(),
        target: "aarch64-unknown-none-softfloat".to_string(),
        smp: None,
        debug: false,
        build_info_path: config_path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    })
    .unwrap();

    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"axvm/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-driver/plat-dyn".to_string()));
}

#[test]
fn load_cargo_config_rejects_removed_nested_axstd_platform_feature() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["ax-std/x86-qemu-q35", "ept-level-4"]
log = "Info"
"#,
    )
    .unwrap();

    let err = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "x86_64".to_string(),
        target: "x86_64-unknown-none".to_string(),
        smp: None,
        debug: false,
        build_info_path: config_path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    })
    .unwrap_err();

    assert!(err.to_string().contains("has been removed"));
}

#[test]
fn load_cargo_config_rejects_removed_x86_q35_platform_feature() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["ax-hal/x86-qemu-q35", "ept-level-4"]
log = "Info"
"#,
    )
    .unwrap();

    let err = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "x86_64".to_string(),
        target: "x86_64-unknown-none".to_string(),
        smp: None,
        debug: false,
        build_info_path: config_path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    })
    .unwrap_err();

    assert!(err.to_string().contains("has been removed"));
}

#[test]
fn load_cargo_config_rejects_removed_sg2002_platform_feature() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    let removed_sg2002_platform = concat!("ax-hal/", "riscv64", "-sg2002");
    fs::write(
        &config_path,
        format!("features = [\"{removed_sg2002_platform}\", \"fs\"]\nlog = \"Info\"\n"),
    )
    .unwrap();

    let err = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "riscv64".to_string(),
        target: "riscv64gc-unknown-none-elf".to_string(),
        smp: None,
        debug: false,
        build_info_path: config_path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    })
    .unwrap_err();

    assert!(err.to_string().contains(removed_sg2002_platform));
}

#[test]
fn load_cargo_config_rejects_direct_axplat_dyn_feature() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["axplat-dyn/efi", "ept-level-4"]
log = "Info"
"#,
    )
    .unwrap();

    let err = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "loongarch64".to_string(),
        target: "loongarch64-unknown-none-softfloat".to_string(),
        smp: None,
        debug: false,
        build_info_path: config_path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    })
    .unwrap_err();

    assert!(err.to_string().contains("dynamic platform features"));
    assert!(err.to_string().contains("axplat-dyn/efi"));
}

#[test]
fn load_cargo_config_uses_dynamic_x86_platform_from_board_config() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["ax-driver/virtio-blk", "ept-level-4", "fs", "vmx"]
log = "Info"
"#,
    )
    .unwrap();

    let cargo = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "x86_64".to_string(),
        target: "x86_64-unknown-none".to_string(),
        smp: None,
        debug: false,
        build_info_path: config_path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    })
    .unwrap();

    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"axvm/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-driver/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"dyn-plat".to_string()));
    assert!(!cargo.features.contains(&"ax-hal/x86-pc".to_string()));
    assert!(!cargo.features.contains(&"ax-hal/x86-qemu-q35".to_string()));
    let removed_static_driver_feature = concat!("ax-driver/", "plat", "-static");
    assert!(
        !cargo
            .features
            .contains(&removed_static_driver_feature.to_string())
    );
}

#[test]
fn load_cargo_config_defaults_x86_to_dynamic_platform_when_omitted() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["ept-level-4", "fs", "vmx"]
log = "Info"
"#,
    )
    .unwrap();

    let cargo = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "x86_64".to_string(),
        target: "x86_64-unknown-none".to_string(),
        smp: None,
        debug: false,
        build_info_path: config_path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    })
    .unwrap();

    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"axvm/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-driver/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-hal/x86-qemu-q35".to_string()));
    assert!(!cargo.features.contains(&"ax-hal/x86-pc".to_string()));
}

#[test]
fn load_cargo_config_applies_stack_protector_from_makefile_features() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _features = TempEnvVar::set("FEATURES", "stack-protector");
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["ept-level-4", "fs", "vmx"]
log = "Info"
"#,
    )
    .unwrap();

    let cargo = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "x86_64".to_string(),
        target: "x86_64-unknown-none".to_string(),
        smp: None,
        debug: false,
        build_info_path: config_path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    })
    .unwrap();

    assert!(
        cargo
            .features
            .contains(&"ax-std/stack-protector".to_string())
    );
    let config = fs::read_to_string(cargo.extra_config.unwrap()).unwrap();
    assert!(config.contains(r#""-Zstack-protector=strong""#));
}

#[test]
fn load_cargo_config_keeps_loongarch_dynamic_axvisor_as_elf() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["ept-level-4"]
log = "Info"
"#,
    )
    .unwrap();

    let cargo = load_cargo_config(&ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "loongarch64".to_string(),
        target: "loongarch64-unknown-none-softfloat".to_string(),
        smp: None,
        debug: false,
        build_info_path: config_path,
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    })
    .unwrap();

    assert!(!cargo.to_bin);
    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"axvm/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-driver/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"axplat-dyn/efi".to_string()));
    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/loongarch64-unknown-linux-musl.json")
    );
}
