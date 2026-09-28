use std::{collections::HashMap, fs, path::PathBuf};

use tempfile::tempdir;

use super::*;
use crate::{
    context::{ResolvedStarryRequest, STARRY_PACKAGE, WorkspaceContext},
    starry::build::LogLevel,
};

fn workspace() -> WorkspaceContext {
    WorkspaceContext::discover(None).unwrap()
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
    assert!(cargo.features.iter().any(|feature| feature == "net"));
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

    let err = load_cargo_config(&request, &workspace()).unwrap_err();

    assert!(
        err.to_string()
            .contains("feature `plat-dyn` is no longer supported"),
        "{err:#}"
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
fn load_cargo_config_rejects_std_compat_for_freestanding_kernel() {
    for feature in ["std-compat", "ax-std/std-compat"] {
        let target = "x86_64-unknown-none";
        let mut request = request(PathBuf::from("/tmp/.build.toml"), "x86_64", target);
        request.build_info_override = Some(StarryBuildInfo {
            features: vec![feature.to_string()],
            ..default_starry_build_info()
        });

        let err = load_cargo_config(&request, &workspace()).unwrap_err();
        assert!(err.to_string().contains("freestanding no_std build"));
    }
}

#[test]
fn riscv_image_header_accepts_compact_entry_and_fixed_offsets() {
    let image = riscv_image_fixture(0x0032_2297, 0x4982_8067);

    validate_riscv_image_header(&image).unwrap();
}

#[test]
fn riscv_image_header_rejects_legacy_sixteen_byte_entry() {
    let mut image = vec![0_u8; 0x80];
    let image_size = image.len() as u64;
    put_u32(&mut image, 0x00, 0x0032_2297);
    put_u32(&mut image, 0x04, 0x0002_8293); // addi t0, t0, 0 from the old lla expansion
    put_u32(&mut image, 0x08, 0x4982_8067);
    put_u32(&mut image, 0x0c, 0x0000_0013); // nop
    put_u64(&mut image, 0x10, 0x20_0000);
    put_u64(&mut image, 0x18, image_size);
    image[0x38..0x3d].copy_from_slice(b"RISCV");
    image[0x40..0x44].copy_from_slice(b"RSC\x05");

    let error = validate_riscv_image_header(&image).unwrap_err();

    assert!(error.to_string().contains("code1"), "{error:#}");
}

fn riscv_image_fixture(code0: u32, code1: u32) -> Vec<u8> {
    let mut image = vec![0_u8; 0x80];
    let image_size = image.len() as u64;
    put_u32(&mut image, 0x00, code0);
    put_u32(&mut image, 0x04, code1);
    put_u64(&mut image, 0x08, 0x20_0000);
    put_u64(&mut image, 0x10, image_size);
    image[0x30..0x35].copy_from_slice(b"RISCV");
    image[0x38..0x3c].copy_from_slice(b"RSC\x05");
    image
}

fn put_u32(image: &mut [u8], offset: usize, value: u32) {
    image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(image: &mut [u8], offset: usize, value: u64) {
    image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
// weave: run 'weave explain scripts/axbuild/src/starry/build/tests.rs' for per-hunk detail, 'weave check' to verify your resolution
