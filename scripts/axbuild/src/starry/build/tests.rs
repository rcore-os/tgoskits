use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use tempfile::tempdir;

use super::*;
use crate::{
    context::{ResolvedStarryRequest, STARRY_PACKAGE, find_workspace_root},
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
fn starry_manifest_disables_std_compat_and_default_tls() {
    let manifest =
        fs::read_to_string(find_workspace_root().join("os/StarryOS/starryos/Cargo.toml")).unwrap();

    assert!(manifest.contains("default-features = false"));
    assert!(!manifest.contains("\"std-compat\""));
    assert!(!manifest.contains("\"tls\""));
}

#[test]
fn starry_kernel_test_manifest_forwards_the_smp_capability() {
    let manifest =
        fs::read_to_string(find_workspace_root().join("os/StarryOS/kernel/Cargo.toml")).unwrap();
    let manifest: toml::Value = toml::from_str(&manifest).unwrap();
    let smp_features = manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .and_then(|features| features.get("smp"))
        .and_then(toml::Value::as_array)
        .expect("starry-kernel must expose the SMP capability")
        .iter()
        .map(|feature| feature.as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(smp_features, ["ax-std/smp"]);
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

    assert_eq!(build_info, default_starry_build_info());
    assert!(path.exists());
    let persisted: StarryBuildInfo = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(persisted, build_info);
}

#[test]
fn default_starry_build_info_does_not_inject_features() {
    let build_info = default_starry_build_info();
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
        ..default_starry_build_info()
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
    assert!(!cargo.to_bin);
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
    let build_info = default_starry_build_info();
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
fn load_cargo_config_rejects_removed_dynamic_platform_feature() {
    let mut request = request(
        PathBuf::from("/tmp/.build.toml"),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    );
    request.build_info_override = Some(StarryBuildInfo {
        env: HashMap::new(),
        features: vec![
            "common".to_string(),
            "plat-dyn".to_string(),
            "ax-driver/rockchip-soc".to_string(),
            "ax-driver/rockchip-sdhci".to_string(),
        ],
        log: LogLevel::Info,
        max_cpu_num: Some(8),
    });

    let err = load_cargo_config(&request).unwrap_err();

    assert!(
        err.to_string()
            .contains("feature `plat-dyn` is no longer supported"),
        "{err:#}"
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
        features: vec!["qemu".to_string()],
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
        features: vec!["axplat-dyn/efi".to_string()],
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
            "starry-kernel/sg2002".to_string(),
            "axplat-dyn/thead-mae".to_string(),
        ],
        ..default_starry_build_info()
    });

    let cargo = load_cargo_config(&request).unwrap();
    let removed_sg2002_platform = concat!("ax-hal/", "riscv64", "-sg2002");

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
    let target = "aarch64-unknown-none-softfloat";
    let mut request = request(PathBuf::from("/tmp/.build.toml"), "aarch64", target);
    request.build_info_override = Some(StarryBuildInfo {
        features: vec!["ax-driver/virtio-blk".to_string()],
        ..default_starry_build_info()
    });

    let cargo = load_cargo_config(&request).unwrap();

    assert_eq!(cargo.target, target);
}

#[test]
fn load_cargo_config_keeps_starry_smp_capability_for_single_or_unspecified_cpu_limits() {
    for requested_smp in [None, Some(1)] {
        let target = "riscv64gc-unknown-none-elf";
        let mut request = request(PathBuf::from("/tmp/.build.toml"), "riscv64", target);
        request.smp = requested_smp;
        request.build_info_override = Some(default_starry_build_info());

        let cargo = load_cargo_config(&request).unwrap();

        assert!(cargo.features.contains(&"smp".to_string()));
        match requested_smp {
            Some(cpu_count) => assert_eq!(cargo.env.get("SMP"), Some(&cpu_count.to_string())),
            None => assert!(!cargo.env.contains_key("SMP")),
        }
    }
}

#[test]
fn load_cargo_config_uses_bare_no_std_pie_contract() {
    let target = "aarch64-unknown-none-softfloat";
    let mut request = request(PathBuf::from("/tmp/.build.toml"), "aarch64", target);
    request.build_info_override = Some(default_starry_build_info());

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
        request.build_info_override = Some(default_starry_build_info());

        let cargo = load_cargo_config(&request).unwrap();
        assert_eq!(cargo.target, target);
        assert_eq!(cargo.to_bin, expected_to_bin);
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
        request.build_info_override = Some(default_starry_build_info());

        let cargo = load_cargo_config(&request).unwrap();
        assert!(cargo.args.join("\n").contains(expected_flag));
    }
}

#[test]
fn load_cargo_config_rejects_std_compat_for_freestanding_kernel() {
    for feature in ["std-compat", "ax-std/std-compat"] {
        let target = "x86_64-unknown-none";
        let mut request = request(PathBuf::from("/tmp/.build.toml"), "x86_64", target);
        request.build_info_override = Some(StarryBuildInfo {
            features: vec![feature.to_string()],
            ..default_starry_build_info()
        });

        let err = load_cargo_config(&request).unwrap_err();
        assert!(err.to_string().contains("freestanding no_std build"));
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
    let build_info = default_starry_build_info();
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
