use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

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
    assert!(!build_info.features.contains(&"qemu".to_string()));
}

#[test]
fn default_riscv64_starry_build_info_uses_dynamic_platform() {
    let build_info = default_starry_build_info_for_target("riscv64gc-unknown-none-elf");
    assert!(!build_info.features.contains(&"qemu".to_string()));
}

#[test]
fn default_x86_starry_build_info_uses_dynamic_platform() {
    let build_info = default_starry_build_info_for_target("x86_64-unknown-none");
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
fn load_target_from_plain_build_config_returns_none() {
    let root = tempdir().unwrap();
    let path = root.path().join(".build-target.toml");
    fs::write(
        &path,
        r#"
features = ["net"]
log = "Info"
"#,
    )
    .unwrap();

    assert_eq!(load_target_from_build_config(&path).unwrap(), None);
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
fn patch_starry_cargo_config_keeps_dynamic_platform_without_qemu() {
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
    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"qemu".to_string()));
    assert!(!cargo.env.contains_key("AX_PLATFORM"));
    assert_eq!(
        cargo.target,
        "scripts/targets/std/pie/aarch64-unknown-linux-musl.json"
    );
}

#[test]
fn patch_starry_cargo_config_keeps_qemu_as_capability_feature() {
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
    };
    let mut cargo = build_info.into_base_cargo_config_with_log(
        STARRY_PACKAGE.to_string(),
        "scripts/targets/std/pie/aarch64-unknown-linux-musl.json".to_string(),
        Vec::new(),
    );

    let metadata = crate::build::workspace_metadata().unwrap();
    patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(cargo.features.contains(&"qemu".to_string()));
    assert!(!cargo.env.contains_key("AX_PLATFORM"));
}

#[test]
fn patch_starry_cargo_config_keeps_loongarch64_dynamic_platform_dynamic() {
    let request = request(
        PathBuf::from("/tmp/.build.toml"),
        "loongarch64",
        "loongarch64-unknown-none-softfloat",
    );
    let build_info = StarryBuildInfo {
        env: HashMap::new(),
        features: vec!["ax-hal/plat-dyn".to_string(), "axplat-dyn/efi".to_string()],
        log: LogLevel::Info,
        max_cpu_num: None,
    };
    let mut cargo = build_info.into_base_cargo_config_with_log(
        STARRY_PACKAGE.to_string(),
        request.target.clone(),
        vec![],
    );
    let metadata = crate::build::workspace_metadata().unwrap();
    patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

    assert!(!cargo.features.contains(&"qemu".to_string()));
    assert!(!cargo.features.contains(&"ax-hal/plat-dyn".to_string()));
    assert!(cargo.features.contains(&"axplat-dyn/efi".to_string()));
    assert!(!cargo.env.contains_key("AX_PLATFORM"));
}

#[test]
fn uimage_its_path_for_config_uses_same_basename_with_its_extension() {
    let path = uimage_its_path_for_config(Path::new("os/StarryOS/configs/board/foo.toml"));

    assert_eq!(path, PathBuf::from("os/StarryOS/configs/board/foo.its"));
}

#[test]
fn uimage_generation_plan_is_absent_without_companion_its() {
    let root = tempdir().unwrap();
    let config = root.path().join("board/foo.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "target = \"riscv64gc-unknown-none-elf\"\n").unwrap();
    let elf = root.path().join("target/kernel.elf");

    assert!(
        uimage_generation_plan(&config, "riscv64", "riscv64gc-unknown-none-elf", &elf).is_none()
    );
}

#[test]
fn uimage_generation_plan_uses_mkimage_f_for_companion_its() {
    let root = tempdir().unwrap();
    let config = root.path().join("board/foo.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "target = \"riscv64gc-unknown-none-elf\"\n").unwrap();
    fs::write(
        root.path().join("board/foo.its"),
        "kernel = \"${kernel_bin}\";\n",
    )
    .unwrap();
    let elf = root.path().join("target/kernel.elf");

    let plan = uimage_generation_plan(&config, "riscv64", "riscv64gc-unknown-none-elf", &elf)
        .expect("companion ITS should request uImage generation");

    assert_eq!(plan.source_its, root.path().join("board/foo.its"));
    assert_eq!(
        plan.rendered_its.parent(),
        Some(root.path().join("target").as_path())
    );
    let rendered_name = plan.rendered_its.file_name().unwrap().to_string_lossy();
    assert!(rendered_name.starts_with(".kernel.elf.uimage.its."));
    assert!(rendered_name.ends_with(".tmp"));
    assert_eq!(plan.kernel_bin, root.path().join("target/kernel.bin"));
    assert_eq!(plan.output_uimg, root.path().join("target/kernel.uimg"));
    assert_eq!(
        mkimage_args_for_its(&plan.rendered_its, &plan.output_uimg),
        vec![
            "-f".to_string(),
            plan.rendered_its.display().to_string(),
            plan.output_uimg.display().to_string(),
        ]
    );
}

#[test]
fn render_uimage_its_template_replaces_build_placeholders() {
    let root = tempdir().unwrap();
    let template = root.path().join("foo.its");
    let rendered = root.path().join("rendered.its");
    let kernel_elf = root.path().join("target/kernel.elf");
    let kernel_bin = root.path().join("target/kernel.bin");
    fs::write(
        &template,
        "bin=${kernel_bin}\nelf=${kernel_elf}\narch=${arch}\ntarget=${target}\n",
    )
    .unwrap();

    render_uimage_its_template(
        &template,
        &rendered,
        &kernel_elf,
        &kernel_bin,
        "riscv64",
        "riscv64gc-unknown-none-elf",
    )
    .unwrap();

    let output = fs::read_to_string(rendered).unwrap();
    assert!(output.contains(&format!("bin={}", kernel_bin.display())));
    assert!(output.contains(&format!("elf={}", kernel_elf.display())));
    assert!(output.contains("arch=riscv64"));
    assert!(output.contains("target=riscv64gc-unknown-none-elf"));
    assert!(!output.contains("${"));
}

#[test]
fn load_cargo_config_keeps_sg2002_as_device_feature_without_static_platform_alias() {
    let mut request = request(
        PathBuf::from("/tmp/.build.toml"),
        "riscv64",
        "riscv64gc-unknown-none-elf",
    );
    request.build_info_override = Some(StarryBuildInfo {
        features: vec![
            "plat-dyn".to_string(),
            "starry-kernel/sg2002".to_string(),
            "axplat-dyn/thead-mae".to_string(),
        ],
        ..default_starry_build_info_for_target("riscv64gc-unknown-none-elf")
    });

    let cargo = load_cargo_config(&request).unwrap();
    let removed_sg2002_platform = concat!("ax-hal/", "riscv64", "-sg2002");

    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(cargo.features.contains(&"starry-kernel/sg2002".to_string()));
    assert!(
        cargo
            .features
            .iter()
            .all(|feature| feature != removed_sg2002_platform)
    );
    assert!(
        !cargo
            .features
            .iter()
            .any(|feature| feature.starts_with("qemu"))
    );
    assert!(!cargo.features.contains(&"qemu".to_string()));
    assert_eq!(cargo.env.get("AX_PLATFORM"), None);
}

#[test]
fn load_cargo_config_keeps_original_bare_target_for_dynamic_platform_request() {
    let mut request = request(
        PathBuf::from("/tmp/.build.toml"),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    );
    request.build_info_override = Some(StarryBuildInfo {
        features: vec!["ax-driver/virtio-blk".to_string()],
        ..default_starry_build_info_for_target("aarch64-unknown-none-softfloat")
    });

    let cargo = load_cargo_config(&request).unwrap();

    assert_eq!(cargo.target, "aarch64-unknown-none-softfloat");
}

#[test]
fn load_cargo_config_keeps_starry_smp_capability_for_single_or_unspecified_cpu_limits() {
    for requested_smp in [None, Some(1)] {
        let target = "riscv64gc-unknown-none-elf";
        let mut request = request(PathBuf::from("/tmp/.build.toml"), "riscv64", target);
        request.smp = requested_smp;
        request.build_info_override = Some(default_starry_build_info_for_target(target));

        let cargo = load_cargo_config(&request).unwrap();

        assert!(
            cargo.features.contains(&"smp".to_string()),
            "Starry must remain SMP-capable when the requested CPU limit is {requested_smp:?}"
        );
        match requested_smp {
            Some(cpu_count) => {
                assert_eq!(cargo.env.get("SMP"), Some(&cpu_count.to_string()));
            }
            None => assert!(!cargo.env.contains_key("SMP")),
        }
    }
}

#[test]
fn load_cargo_config_uses_bare_no_std_pie_contract() {
    let target = "aarch64-unknown-none-softfloat";
    let mut request = request(PathBuf::from("/tmp/.build.toml"), "aarch64", target);
    request.build_info_override = Some(default_starry_build_info_for_target(target));

    let cargo = load_cargo_config(&request).unwrap();
    let args = cargo.args.join("\n");

    assert_eq!(cargo.target, target);
    assert!(
        cargo
            .args
            .windows(2)
            .any(|pair| pair == ["-Z", "build-std=core,alloc"])
    );
    for flag in [
        "-Crelocation-model=pic",
        "-Clink-args=-pie",
        "-Clink-args=--gc-sections",
        "-Clink-args=-znorelro",
        "-Clink-args=-znostart-stop-gc",
        "-Clink-args=-Tlinker.x",
        "-Clink-args=-u _head",
    ] {
        assert!(args.contains(flag), "missing Starry PIE rustflag {flag}");
    }
    assert!(cargo.extra_config.is_none());
    assert!(cargo.pre_build_cmds.is_empty());
    assert!(!cargo.target.contains("scripts/targets/std"));
    assert!(!cargo.args.iter().any(|arg| arg == "json-target-spec"));
    assert!(!cargo.env.contains_key("CARGO_UNSTABLE_JSON_TARGET_SPEC"));
    assert!(
        cargo
            .features
            .iter()
            .all(|feature| !feature.contains("std-compat"))
    );
}

#[test]
fn load_cargo_config_derives_to_bin_from_original_bare_target() {
    for (arch, target, expected_to_bin) in [
        ("x86_64", "x86_64-unknown-none", false),
        ("loongarch64", "loongarch64-unknown-none-softfloat", false),
        ("aarch64", "aarch64-unknown-none-softfloat", true),
        ("riscv64", "riscv64gc-unknown-none-elf", true),
    ] {
        let mut request = request(PathBuf::from("/tmp/.build.toml"), arch, target);
        request.build_info_override = Some(default_starry_build_info_for_target(target));

        let cargo = load_cargo_config(&request).unwrap();

        assert_eq!(cargo.target, target);
        assert_eq!(
            cargo.to_bin, expected_to_bin,
            "unexpected artifact conversion for original target {target}"
        );
    }
}

#[test]
fn load_cargo_config_applies_arch_specific_bare_pie_flags() {
    for (arch, target, expected_flag) in [
        (
            "riscv64",
            "riscv64gc-unknown-none-elf",
            "-Clink-args=--no-relax",
        ),
        (
            "loongarch64",
            "loongarch64-unknown-none-softfloat",
            "-Ctarget-feature=-ual",
        ),
    ] {
        let mut request = request(PathBuf::from("/tmp/.build.toml"), arch, target);
        request.build_info_override = Some(default_starry_build_info_for_target(target));

        let cargo = load_cargo_config(&request).unwrap();

        assert!(
            cargo.args.join("\n").contains(expected_flag),
            "missing architecture rustflag {expected_flag} for {target}"
        );
    }
}

#[test]
fn load_cargo_config_rejects_std_compat_for_freestanding_kernel() {
    for feature in ["std-compat", "ax-std/std-compat"] {
        let target = "x86_64-unknown-none";
        let mut request = request(PathBuf::from("/tmp/.build.toml"), "x86_64", target);
        request.build_info_override = Some(StarryBuildInfo {
            features: vec![feature.to_string()],
            ..default_starry_build_info_for_target(target)
        });

        let err = load_cargo_config(&request).unwrap_err();

        assert!(
            err.to_string().contains("freestanding no_std build"),
            "unexpected error for {feature}: {err:#}"
        );
    }
}

#[test]
fn load_cargo_config_rejects_kernel_tls_register_mode() {
    for feature in [
        "tls",
        "ax-std/tls",
        "ax-runtime/tls",
        "ax-hal/tls",
        "ax-cpu-local/tls",
        "someboot/tls",
    ] {
        let target = "x86_64-unknown-none";
        let mut request = request(PathBuf::from("/tmp/.build.toml"), "x86_64", target);
        request.build_info_override = Some(StarryBuildInfo {
            features: vec![feature.to_string()],
            ..default_starry_build_info_for_target(target)
        });

        let err = load_cargo_config(&request).unwrap_err();

        assert!(
            err.to_string().contains("Starry LinuxCurrent"),
            "unexpected error for {feature}: {err:#}"
        );
    }
}

#[test]
fn starry_kernel_entry_is_freestanding_c_abi() {
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../os/StarryOS/starryos/src/main.rs"),
    )
    .unwrap();

    assert!(source.contains("#![no_std]"));
    assert!(source.contains("#![no_main]"));
    assert!(source.contains("extern \"C\" fn main()"));
    assert!(!source.contains("cfg_attr(target_os"));
}

#[test]
fn freestanding_elf_audit_accepts_a_bare_dynamic_image() {
    let elf = minimal_elf64_with_program_header(object::elf::PT_LOAD);

    audit_freestanding_starry_elf_bytes(&elf, Path::new("bare-starry.elf")).unwrap();
}

#[test]
fn freestanding_elf_audit_rejects_a_tls_program_header() {
    let elf = minimal_elf64_with_program_header(object::elf::PT_TLS);

    let error = audit_freestanding_starry_elf_bytes(&elf, Path::new("tls-starry.elf")).unwrap_err();

    assert!(error.to_string().contains("PT_TLS"));
}

#[test]
fn freestanding_elf_audit_rejects_tls_sections_and_std_symbols() {
    for section in [".tdata", ".tbss"] {
        assert!(is_forbidden_starry_section(section));
    }
    for symbol in [
        "std::rt::lang_start_internal",
        "std::panicking::LOCAL_PANIC_COUNT",
        "_ZN3std9panicking17LOCAL_PANIC_COUNT17h1234567890abcdefE",
    ] {
        assert!(is_forbidden_starry_symbol(symbol));
    }
    assert!(!is_forbidden_starry_section(".data"));
    assert!(!is_forbidden_starry_symbol("core::panicking::panic_fmt"));
}

#[test]
fn final_freestanding_elf_is_audited_after_kallsyms_before_binary_refresh() {
    let source = include_str!("../build.rs");
    let postprocess = source
        .split_once("pub(crate) fn postprocess_starry_artifact")
        .expect("Starry artifact post-processing must remain explicit")
        .1
        .split_once("fn audit_freestanding_starry_elf")
        .expect("the final ELF audit must remain a focused helper")
        .0;
    let kallsyms = postprocess
        .find("generate_kallsyms(elf)")
        .expect("kallsyms must be generated into the final ELF");
    let audit = postprocess
        .find("audit_freestanding_starry_elf(elf)")
        .expect("the final ELF must be audited");
    let refresh = postprocess
        .find("refresh_bin_if_present(elf)")
        .expect("binary conversion must consume the audited final ELF");
    assert!(
        kallsyms < audit && audit < refresh,
        "the audit must inspect the post-kallsyms image that is converted and executed"
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

fn minimal_elf64_with_program_header(program_type: u32) -> Vec<u8> {
    const ELF_HEADER_SIZE: usize = 64;
    const PROGRAM_HEADER_SIZE: usize = 56;
    let mut elf = vec![0_u8; ELF_HEADER_SIZE + PROGRAM_HEADER_SIZE];
    elf[..16].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    elf[16..18].copy_from_slice(&object::elf::ET_DYN.to_le_bytes());
    elf[18..20].copy_from_slice(&object::elf::EM_X86_64.to_le_bytes());
    elf[20..24].copy_from_slice(&1_u32.to_le_bytes());
    elf[32..40].copy_from_slice(&(ELF_HEADER_SIZE as u64).to_le_bytes());
    elf[52..54].copy_from_slice(&(ELF_HEADER_SIZE as u16).to_le_bytes());
    elf[54..56].copy_from_slice(&(PROGRAM_HEADER_SIZE as u16).to_le_bytes());
    elf[56..58].copy_from_slice(&1_u16.to_le_bytes());
    elf[58..60].copy_from_slice(&64_u16.to_le_bytes());

    elf[ELF_HEADER_SIZE..ELF_HEADER_SIZE + 4].copy_from_slice(&program_type.to_le_bytes());
    elf[ELF_HEADER_SIZE + 4..ELF_HEADER_SIZE + 8].copy_from_slice(&object::elf::PF_R.to_le_bytes());
    elf[ELF_HEADER_SIZE + 48..ELF_HEADER_SIZE + 56].copy_from_slice(&8_u64.to_le_bytes());
    elf
}
