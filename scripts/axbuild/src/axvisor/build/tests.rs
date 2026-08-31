use std::{
    fs,
    path::{Path, PathBuf},
};

use tempfile::tempdir;

use super::*;

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
fn axvisor_all_architectures_use_rust_std_musl_with_abort_panics() {
    let cases = [
        (
            "x86_64",
            "x86_64-unknown-none",
            "x86_64-unknown-linux-musl.json",
        ),
        (
            "aarch64",
            "aarch64-unknown-none-softfloat",
            "aarch64-unknown-linux-musl.json",
        ),
        (
            "riscv64",
            "riscv64gc-unknown-none-elf",
            "riscv64gc-unknown-linux-musl.json",
        ),
        (
            "loongarch64",
            "loongarch64-unknown-none-softfloat",
            "loongarch64-unknown-linux-musl.json",
        ),
    ];

    for (arch, target, std_target) in cases {
        let root = tempdir().unwrap();
        let config_path = root.path().join(format!(".{arch}-build.toml"));
        fs::write(&config_path, "features = []\nlog = \"Info\"\n").unwrap();

        let cargo = load_cargo_config(&request(config_path, arch, target)).unwrap();
        assert!(
            cargo
                .target
                .ends_with(&format!("scripts/targets/std/pie/{std_target}")),
            "{arch} Axvisor must use its RustStd/musl PIE target"
        );

        let config: toml::Table =
            toml::from_str(&fs::read_to_string(cargo.extra_config.unwrap()).unwrap()).unwrap();
        assert_eq!(
            config["unstable"]["build-std"].as_array().unwrap(),
            &vec![
                toml::Value::String("std".to_string()),
                toml::Value::String("panic_abort".to_string()),
            ],
            "{arch} Axvisor must build real Rust std and panic_abort"
        );
        assert_eq!(
            config["profile"]["release"]["panic"].as_str(),
            Some("abort"),
            "{arch} Axvisor release profile must abort on panic"
        );
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
features = []
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
    for vmconfig in &vmconfigs {
        fs::write(vmconfig, "[kernel]\n").unwrap();
    }
    fs::write(
        &config_path,
        r#"
features = ["fs"]
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
fn load_cargo_config_does_not_select_an_x86_backend() {
    let root = tempdir().unwrap();
    let config_path = root.path().join("build-x86_64.toml");
    fs::write(
        &config_path,
        r#"
features = []
log = "Info"
"#,
    )
    .unwrap();

    let cargo = load_cargo_config(&request(config_path, "x86_64", "x86_64-unknown-none")).unwrap();

    assert!(!cargo.features.contains(&"vmx".to_string()));
    assert!(!cargo.features.contains(&"svm".to_string()));
}

#[test]
fn load_cargo_config_rejects_explicit_x86_backend_features() {
    for feature in ["vmx", "svm"] {
        let root = tempdir().unwrap();
        let config_path = root.path().join(format!("build-x86_64-{feature}.toml"));
        fs::write(
            &config_path,
            format!(
                r#"
features = ["{feature}"]
log = "Info"
"#
            ),
        )
        .unwrap();

        let err =
            load_cargo_config(&request(config_path, "x86_64", "x86_64-unknown-none")).unwrap_err();

        assert!(err.to_string().contains("selected from CPU capabilities"));
        assert!(err.to_string().contains(&format!("`{feature}`")));
    }
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
features = ["fs"]
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
    assert!(cargo.features.contains(&"fs".to_string()));
    assert!(!cargo.features.contains(&"vmx".to_string()));
    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"axvm/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-driver/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/x86-pc".to_string()));
    assert!(!cargo.features.contains(&"ax-hal/x86-pc".to_string()));
}

#[test]
fn load_cargo_config_defaults_aarch64_to_dynamic_platform() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["ax-std"]
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
fn load_cargo_config_rejects_direct_axplat_dyn_feature() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["axplat-dyn/efi"]
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
features = ["ax-driver/nvme", "fs"]
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
features = ["fs"]
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
    assert!(!cargo.features.contains(&"ax-hal/x86-pc".to_string()));
}

#[test]
fn load_cargo_config_applies_stack_protector_from_makefile_features() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["fs"]
log = "Info"
"#,
    )
    .unwrap();

    let cargo = load_cargo_config_with_makefile_features(
        &ResolvedAxvisorRequest {
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
        },
        &["stack-protector".to_string()],
    )
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
fn load_cargo_config_prepares_loongarch_dynamic_axvisor_runtime_artifact() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = []
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
