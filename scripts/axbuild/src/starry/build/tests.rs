use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use ostool::build::config::Cargo;
use tempfile::tempdir;

use super::*;
use crate::{
    context::{ResolvedStarryRequest, STARRY_PACKAGE},
    starry::build::LogLevel,
};

fn write_minimal_package_manifest(path: &Path, name: &str) {
    let src_dir = path.parent().unwrap().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("lib.rs"), "").unwrap();
    fs::write(
        path,
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .unwrap();
}

fn request(path: PathBuf, arch: &str, target: &str) -> ResolvedStarryRequest {
    ResolvedStarryRequest {
        package: STARRY_PACKAGE.to_string(),
        arch: arch.to_string(),
        target: target.to_string(),
        plat_dyn: None,
        smp: None,
        debug: false,
        build_info_path: path,
        build_info_override: None,
        qemu_config: None,
        uboot_config: None,
    }
}

#[test]
fn resolve_build_info_path_uses_default_starry_location() {
    let root = tempdir().unwrap();
    let starry_dir = root.path().join("os/StarryOS/starryos");
    fs::create_dir_all(&starry_dir).unwrap();
    write_minimal_package_manifest(&starry_dir.join("Cargo.toml"), STARRY_PACKAGE);
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
    )
    .unwrap();
    let path =
        resolve_build_info_path(root.path(), "aarch64-unknown-none-softfloat", None).unwrap();

    assert_eq!(
        path,
        root.path()
            .join("tmp/axbuild/config/starryos/build-aarch64-unknown-none-softfloat.toml")
    );
}

#[test]
fn resolve_build_info_path_ignores_source_tree_defaults() {
    let root = tempdir().unwrap();
    let starry_dir = root.path().join("os/StarryOS/starryos");
    fs::create_dir_all(&starry_dir).unwrap();
    write_minimal_package_manifest(&starry_dir.join("Cargo.toml"), STARRY_PACKAGE);
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
    )
    .unwrap();
    let bare = starry_dir.join("build-aarch64-unknown-none-softfloat.toml");
    let dotted = starry_dir.join(".build-aarch64-unknown-none-softfloat.toml");
    fs::write(&bare, "").unwrap();
    fs::write(&dotted, "").unwrap();

    let path =
        resolve_build_info_path(root.path(), "aarch64-unknown-none-softfloat", None).unwrap();

    assert_eq!(
        path,
        root.path()
            .join("tmp/axbuild/config/starryos/build-aarch64-unknown-none-softfloat.toml")
    );
}

#[test]
fn resolve_build_info_path_prefers_explicit_path() {
    let root = tempdir().unwrap();
    let starry_dir = root.path().join("os/StarryOS/starryos");
    fs::create_dir_all(&starry_dir).unwrap();
    write_minimal_package_manifest(&starry_dir.join("Cargo.toml"), STARRY_PACKAGE);
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
    )
    .unwrap();
    let explicit = root.path().join("custom/build.toml");
    let path = resolve_build_info_path(root.path(), "x86_64-unknown-none", Some(explicit.clone()))
        .unwrap();

    assert_eq!(path, explicit);
}

#[test]
fn load_build_info_writes_default_template_when_missing() {
    let root = tempdir().unwrap();
    let path = root.path().join(".build-target.toml");
    let request = request(path.clone(), "aarch64", "aarch64-unknown-none-softfloat");

    let build_info = load_build_info(&request).unwrap();

    assert_eq!(
        build_info,
        default_starry_build_info_for_target("aarch64-unknown-none-softfloat")
    );
    assert!(path.exists());
    let persisted: StarryBuildInfo = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(persisted, build_info);
}

#[test]
fn default_aarch64_starry_build_info_uses_dynamic_platform() {
    let build_info = default_starry_build_info_for_target("aarch64-unknown-none-softfloat");

    assert!(build_info.plat_dyn);
    assert!(!build_info.features.contains(&"qemu".to_string()));
}

#[test]
fn default_riscv64_starry_build_info_uses_dynamic_platform() {
    let build_info = default_starry_build_info_for_target("riscv64gc-unknown-none-elf");

    assert!(build_info.plat_dyn);
    assert!(!build_info.features.contains(&"qemu".to_string()));
}

#[test]
fn default_x86_starry_build_info_uses_dynamic_platform() {
    let build_info = default_starry_build_info_for_target("x86_64-unknown-none");

    assert!(build_info.plat_dyn);
    assert!(build_info.features.is_empty());
}

#[test]
fn load_build_info_reads_existing_file() {
    let root = tempdir().unwrap();
    let path = root.path().join(".build-target.toml");
    fs::write(
        &path,
        r#"
log = "Info"
features = ["net"]

[env]
HELLO = "world"
"#,
    )
    .unwrap();

    let request = request(path, "aarch64", "aarch64-unknown-none-softfloat");
    let build_info = load_build_info(&request).unwrap();

    assert_eq!(build_info.log, LogLevel::Info);
    assert_eq!(build_info.features, vec!["net".to_string()]);
    assert_eq!(
        build_info.env.get("HELLO").map(String::as_str),
        Some("world")
    );
}

#[test]
fn load_target_from_build_config_rejects_removed_std_field() {
    let root = tempdir().unwrap();
    let path = root.path().join(".build-target.toml");
    fs::write(
        &path,
        r#"
std = false
features = []
log = "Info"

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
    let path = root.path().join(".build-target.toml");
    fs::write(
        &path,
        r#"
app-c = "c"
features = []
log = "Info"

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
fn load_build_info_prefers_request_override_without_writing_file() {
    let root = tempdir().unwrap();
    let path = root.path().join(".build-target.toml");
    let mut request = request(path.clone(), "aarch64", "aarch64-unknown-none-softfloat");
    request.build_info_override = Some(StarryBuildInfo {
        log: LogLevel::Info,
        features: vec!["net".to_string()],
        ..default_starry_build_info_for_target("aarch64-unknown-none-softfloat")
    });

    let build_info = load_build_info(&request).unwrap();

    assert_eq!(build_info.log, LogLevel::Info);
    assert_eq!(build_info.features, vec!["net".to_string()]);
    assert!(!path.exists());
}

#[test]
fn patch_starry_cargo_config_injects_required_features_and_env() {
    let request = request(
        PathBuf::from("/tmp/.build.toml"),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    );
    let build_info = StarryBuildInfo {
        env: HashMap::from([(String::from("CUSTOM"), String::from("1"))]),
        features: vec!["net".to_string()],
        log: LogLevel::Info,
        max_cpu_num: None,
        axconfig_overrides: Vec::new(),
        plat_dyn: false,
    };
    let mut cargo = build_info.into_base_cargo_config_with_log(
        STARRY_PACKAGE.to_string(),
        request.target.clone(),
        vec![],
    );
    let metadata = crate::build::workspace_metadata().unwrap();
    patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

    assert_eq!(cargo.package, STARRY_PACKAGE);
    assert_eq!(cargo.target, "aarch64-unknown-none-softfloat");
    assert_eq!(cargo.features, vec!["net".to_string()]);
    assert_eq!(
        cargo.env.get("AX_ARCH").map(String::as_str),
        Some("aarch64")
    );
    assert_eq!(
        cargo.env.get("AX_TARGET").map(String::as_str),
        Some("aarch64-unknown-none-softfloat")
    );
    assert_eq!(cargo.env.get("AX_PLATFORM").map(String::as_str), None);
    assert_eq!(cargo.env.get("AX_LOG").map(String::as_str), Some("info"));
    assert_eq!(cargo.env.get("CUSTOM").map(String::as_str), Some("1"));
    assert!(cargo.to_bin);
    assert!(cargo.post_build_cmds.is_empty());
}

#[test]
fn patch_starry_cargo_config_preserves_request_package() {
    let request = ResolvedStarryRequest {
        package: STARRY_PACKAGE.to_string(),
        arch: "x86_64".to_string(),
        target: "x86_64-unknown-none".to_string(),
        plat_dyn: None,
        smp: None,
        debug: false,
        build_info_path: PathBuf::from("/tmp/.build.toml"),
        build_info_override: None,
        qemu_config: None,
        uboot_config: None,
    };
    let build_info = default_starry_build_info_for_target("x86_64-unknown-none");
    let mut cargo = build_info.into_base_cargo_config_with_log(
        "placeholder".to_string(),
        request.target.clone(),
        vec![],
    );

    let metadata = crate::build::workspace_metadata().unwrap();
    patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

    assert_eq!(cargo.package, STARRY_PACKAGE);
    assert_eq!(
        cargo.args,
        vec!["--bin".to_string(), STARRY_PACKAGE.to_string()]
    );
}

#[test]
fn patch_starry_cargo_config_skips_qemu_for_dynamic_platforms() {
    let request = request(
        PathBuf::from("/tmp/.build.toml"),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    );
    let build_info = StarryBuildInfo {
        env: HashMap::new(),
        features: vec![
            "common".to_string(),
            "plat-dyn".to_string(),
            "ax-driver/rockchip-soc".to_string(),
            "ax-driver/rockchip-sdhci".to_string(),
        ],
        log: LogLevel::Info,
        max_cpu_num: Some(8),
        axconfig_overrides: Vec::new(),
        plat_dyn: true,
    };
    let mut cargo = build_info.into_base_cargo_config_with_log(
        STARRY_PACKAGE.to_string(),
        "scripts/targets/std/pie/aarch64-unknown-linux-musl.json".to_string(),
        Vec::new(),
    );

    let metadata = crate::build::workspace_metadata().unwrap();
    patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

    assert!(
        cargo
            .features
            .contains(&"ax-driver/rockchip-soc".to_string())
    );
    assert!(
        cargo
            .features
            .contains(&"ax-driver/rockchip-sdhci".to_string())
    );
    assert!(!cargo.features.contains(&"qemu".to_string()));
    assert!(!cargo.env.contains_key("AX_PLATFORM"));
    assert_eq!(
        cargo.target,
        "scripts/targets/std/pie/aarch64-unknown-linux-musl.json"
    );
}

#[test]
fn patch_starry_cargo_config_removes_qemu_for_dynamic_platforms() {
    let request = request(
        PathBuf::from("/tmp/.build.toml"),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    );
    let build_info = StarryBuildInfo {
        env: HashMap::new(),
        features: vec!["qemu".to_string(), "plat-dyn".to_string()],
        log: LogLevel::Info,
        max_cpu_num: None,
        axconfig_overrides: Vec::new(),
        plat_dyn: true,
    };
    let mut cargo = build_info.into_base_cargo_config_with_log(
        STARRY_PACKAGE.to_string(),
        "scripts/targets/std/pie/aarch64-unknown-linux-musl.json".to_string(),
        Vec::new(),
    );

    let metadata = crate::build::workspace_metadata().unwrap();
    patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

    assert!(!cargo.features.contains(&"qemu".to_string()));
    assert!(!cargo.env.contains_key("AX_PLATFORM"));
}

#[test]
fn aarch64_qemu_virt_is_not_a_default_static_starry_platform() {
    assert_eq!(
        default_starry_qemu_platform_feature("ax-hal/aarch64-qemu-virt"),
        None
    );
}

#[test]
fn aarch64_has_no_static_starry_default_platform() {
    assert_eq!(
        crate::context::starry_default_platform_for_arch_checked("aarch64").unwrap(),
        None
    );
}

#[test]
fn riscv64_has_no_starry_default_platform() {
    assert_eq!(
        crate::context::starry_default_platform_for_arch_checked("riscv64").unwrap(),
        None
    );
}

#[test]
fn uimage_load_paddr_uses_dynamic_riscv64_fallback_without_axconfig() {
    let cargo = Cargo {
        env: HashMap::new(),
        target: "scripts/targets/std/pie/riscv64gc-unknown-linux-musl.json".to_string(),
        package: STARRY_PACKAGE.to_string(),
        bin: None,
        features: vec!["plat-dyn".to_string()],
        log: None,
        extra_config: None,
        profile: None,
        disable_someboot_build_config: true,
        args: Vec::new(),
        pre_build_cmds: Vec::new(),
        post_build_cmds: Vec::new(),
        to_bin: true,
    };

    assert_eq!(uimage_load_paddr(&cargo, "riscv64").unwrap(), "0x80200000");
}

#[test]
fn uimage_load_paddr_uses_axconfig_when_available() {
    let cargo = Cargo {
        env: HashMap::from([(
            "AX_CONFIG_PATH".to_string(),
            "/tmp/generated.axconfig.toml".to_string(),
        )]),
        target: "riscv64gc-unknown-none-elf".to_string(),
        package: STARRY_PACKAGE.to_string(),
        bin: None,
        features: vec!["plat-dyn".to_string()],
        log: None,
        extra_config: None,
        profile: None,
        disable_someboot_build_config: true,
        args: Vec::new(),
        pre_build_cmds: Vec::new(),
        post_build_cmds: Vec::new(),
        to_bin: true,
    };

    let err = uimage_load_paddr(&cargo, "riscv64").unwrap_err();
    assert!(err.to_string().contains("ax-config-gen"));
}

#[test]
fn load_cargo_config_treats_sg2002_as_explicit_platform_feature() {
    let mut request = request(
        PathBuf::from("/tmp/.build.toml"),
        "riscv64",
        "riscv64gc-unknown-none-elf",
    );
    request.build_info_override = Some(StarryBuildInfo {
        features: vec!["sg2002".to_string()],
        plat_dyn: false,
        ..default_starry_build_info_for_target("riscv64gc-unknown-none-elf")
    });

    let cargo = load_cargo_config(&request).unwrap();

    assert!(cargo.features.contains(&"sg2002".to_string()));
    assert!(
        cargo
            .features
            .contains(&"ax-hal/riscv64-sg2002".to_string())
    );
    assert!(
        !cargo
            .features
            .iter()
            .any(|feature| feature.starts_with("qemu"))
    );
    assert!(!cargo.features.contains(&"qemu".to_string()));
    assert_eq!(
        cargo.env.get("AX_PLATFORM").map(String::as_str),
        Some("riscv64-sg2002")
    );
}

#[test]
fn load_cargo_config_keeps_pie_target_for_non_kmod_plat_dyn_request() {
    let mut request = request(
        PathBuf::from("/tmp/.build.toml"),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    );
    request.build_info_override = Some(StarryBuildInfo {
        plat_dyn: true,
        features: vec!["ax-driver/virtio-blk".to_string()],
        ..default_starry_build_info_for_target("aarch64-unknown-none-softfloat")
    });

    let cargo = load_cargo_config(&request).unwrap();

    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json"),
        "expected pie target, got {}",
        cargo.target
    );
}

#[test]
fn resolve_build_info_path_supports_starry_subworkspace_root() {
    let root = tempdir().unwrap();
    let starry_dir = root.path().join("starryos");
    fs::create_dir_all(&starry_dir).unwrap();
    write_minimal_package_manifest(&starry_dir.join("Cargo.toml"), STARRY_PACKAGE);
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"starryos\"]\n",
    )
    .unwrap();

    let path =
        resolve_build_info_path(root.path(), "aarch64-unknown-none-softfloat", None).unwrap();

    assert_eq!(
        path,
        root.path()
            .join("tmp/axbuild/config/starryos/build-aarch64-unknown-none-softfloat.toml")
    );
}

#[test]
fn patch_starry_cargo_config_preserves_json_target() {
    let request = ResolvedStarryRequest {
        package: STARRY_PACKAGE.to_string(),
        arch: "aarch64".to_string(),
        target: "aarch64-unknown-none-softfloat".to_string(),
        plat_dyn: None,
        smp: None,
        debug: false,
        build_info_path: PathBuf::from(
            "/tmp/os/StarryOS/starryos/.build-aarch64-unknown-none-softfloat.toml",
        ),
        build_info_override: None,
        qemu_config: None,
        uboot_config: None,
    };
    let build_info = default_starry_build_info_for_target(&request.target);
    let mut cargo = build_info.into_base_cargo_config_with_log(
        request.package.clone(),
        "scripts/targets/std/aarch64-unknown-linux-musl.json".to_string(),
        vec!["-Z".to_string(), "json-target-spec".to_string()],
    );

    let metadata = crate::build::workspace_metadata().unwrap();
    patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

    assert_eq!(
        cargo.target,
        "scripts/targets/std/aarch64-unknown-linux-musl.json"
    );
    assert_eq!(cargo.env.get("AX_TARGET"), Some(&request.target));
}

#[test]
fn ensure_starry_bin_arg_adds_bin_for_starryos_package() {
    let mut args = Vec::new();

    let metadata = crate::build::workspace_metadata().unwrap();
    ensure_starry_bin_arg(&mut args, "starryos", &metadata).unwrap();

    assert_eq!(args, vec!["--bin".to_string(), "starryos".to_string()]);
}

#[test]
fn ensure_starry_bin_arg_keeps_existing_bin_arg() {
    let mut args = vec!["--bin".to_string(), "starryos".to_string()];

    let metadata = crate::build::workspace_metadata().unwrap();
    ensure_starry_bin_arg(&mut args, STARRY_PACKAGE, &metadata).unwrap();

    assert_eq!(args, vec!["--bin".to_string(), "starryos".to_string()]);
}

#[test]
fn patch_starry_cargo_config_validates_uimage_without_shell_post_build() {
    let request = request(
        PathBuf::from("/tmp/.build.toml"),
        "riscv64",
        "riscv64gc-unknown-none-elf",
    );
    let mut build_info = default_starry_build_info_for_target(&request.target);
    build_info.env.insert("UIMAGE".to_string(), "y".to_string());
    let mut cargo = build_info.into_base_cargo_config_with_log(
        request.package.clone(),
        request.target.clone(),
        StarryBuildInfo::build_cargo_args(&request.target, &[]),
    );
    cargo.features.push("plat-dyn".to_string());

    let metadata = crate::build::workspace_metadata().unwrap();
    patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

    assert!(cargo.post_build_cmds.is_empty());
}
