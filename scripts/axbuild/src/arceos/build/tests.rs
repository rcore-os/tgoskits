use std::{
    fs,
    path::{Path, PathBuf},
};

use tempfile::tempdir;

use super::{
    ArceosBuildInfo, ArceosBuildMode, default_build_info_path,
    info::{load_build_info, resolve_build_info_path_in_dir},
    load_arceos_build_mode, load_c_app_cargo_config, resolve_app_c_dir, resolve_app_c_mode,
    resolve_build_info_path,
};
use crate::{
    build,
    context::{ResolvedBuildRequest, find_workspace_root},
};

fn repo_metadata() -> cargo_metadata::Metadata {
    build::workspace_metadata().unwrap()
}

fn request(package: &str, target: &str, build_info_path: PathBuf) -> ResolvedBuildRequest {
    ResolvedBuildRequest {
        package: package.to_string(),
        arch: if target.starts_with("x86_64") {
            "x86_64".to_string()
        } else if target.starts_with("aarch64") {
            "aarch64".to_string()
        } else if target.starts_with("riscv64") {
            "riscv64".to_string()
        } else if target.starts_with("loongarch64") {
            "loongarch64".to_string()
        } else {
            "unknown".to_string()
        },
        target: target.to_string(),
        smp: None,
        debug: false,
        build_info_path,
        qemu_config: None,
        uboot_config: None,
    }
}

#[test]
fn build_cargo_args_use_builtin_target_and_build_std() {
    let args = ArceosBuildInfo::build_cargo_args("aarch64-unknown-none-softfloat", &[]);
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-Z", "build-std=core,alloc"])
    );
    assert!(!args.iter().any(|arg| arg.contains("-Clink-arg=-T")));
}

#[test]
fn max_cpu_num_adds_smp_feature_for_std_build() {
    let mut build_info = ArceosBuildInfo {
        features: vec!["ax-api/net".to_string()],
        max_cpu_num: Some(4),
        ..ArceosBuildInfo::default()
    };

    build_info.resolve_c_app_features().unwrap();

    assert!(build_info.features.contains(&"ax-std/smp".to_string()));
}

#[test]
fn arceos_shell_declares_the_filesystem_backend_used_by_its_qemu_disk() {
    let manifest =
        fs::read_to_string(find_workspace_root().join("apps/arceos/shell/Cargo.toml")).unwrap();

    assert!(manifest.contains("ax-std/fatfs"));
}

#[test]
fn resolve_build_info_path_uses_package_directory() {
    let path = resolve_build_info_path("arceos-helloworld", "aarch64-unknown-none-softfloat", None)
        .unwrap();
    let default_path =
        default_build_info_path("arceos-helloworld", "aarch64-unknown-none-softfloat").unwrap();

    assert_eq!(path, default_path);
    assert!(path.ends_with(
        "tmp/axbuild/config/arceos-helloworld/build-aarch64-unknown-none-softfloat.toml"
    ));
}

#[test]
fn resolve_build_info_path_prefers_explicit_path() {
    let path = resolve_build_info_path(
        "arceos-helloworld",
        "aarch64-unknown-none-softfloat",
        Some(PathBuf::from("/tmp/custom-build.toml")),
    )
    .unwrap();

    assert_eq!(path, PathBuf::from("/tmp/custom-build.toml"));
}

#[test]
fn resolve_build_info_path_in_dir_prefers_existing_bare_name() {
    let root = tempdir().unwrap();
    let bare = root
        .path()
        .join("build-aarch64-unknown-none-softfloat.toml");
    let dotted = root
        .path()
        .join(".build-aarch64-unknown-none-softfloat.toml");
    fs::write(&bare, "").unwrap();
    fs::write(&dotted, "").unwrap();

    let path = resolve_build_info_path_in_dir(root.path(), "aarch64-unknown-none-softfloat");

    assert_eq!(path, bare);
}

#[test]
fn load_build_info_creates_missing_default_file() {
    let root = tempdir().unwrap();
    let path = root.path().join(".build-target.toml");
    let request = request("arceos-helloworld", "target", path.clone());

    let build_info = load_build_info(&request).unwrap();

    assert_eq!(build_info, ArceosBuildInfo::default());
    assert!(path.exists());
    assert!(fs::read_to_string(path).unwrap().contains("features = []"));
}

#[test]
fn qemu_build_mode_initializes_missing_configs_for_all_supported_targets() {
    let root = tempdir().unwrap();

    for target in [
        "aarch64-unknown-none-softfloat",
        "x86_64-unknown-none",
        "riscv64gc-unknown-none-elf",
        "loongarch64-unknown-none-softfloat",
    ] {
        let path = root.path().join(format!("build-{target}.toml"));

        let mode = load_arceos_build_mode(&path).unwrap();

        assert_eq!(mode, ArceosBuildMode::RustStd);
        assert!(path.is_file());
    }
}

#[test]
fn build_config_without_app_c_uses_std_rust_mode() {
    let root = tempdir().unwrap();
    let path = root.path().join("build-x86_64-unknown-none.toml");
    fs::write(&path, "features = [\"ax-std\"]\nlog = \"Warn\"\n").unwrap();

    let mode = load_arceos_build_mode(&path).unwrap();

    assert_eq!(mode, ArceosBuildMode::RustStd);
}

#[test]
fn app_c_build_config_resolves_source_dir_relative_to_config() {
    let root = tempdir().unwrap();
    let case_dir = root.path().join("case");
    let source_dir = case_dir.join("c");
    fs::create_dir_all(&source_dir).unwrap();
    fs::write(source_dir.join("main.c"), "int main(void) { return 0; }\n").unwrap();
    let path = case_dir.join("build-x86_64-unknown-none.toml");
    fs::write(&path, "app-c = \"c\"\nfeatures = []\nlog = \"Warn\"\n").unwrap();

    let mode = load_arceos_build_mode(&path).unwrap();
    let app_dir = resolve_app_c_dir(&path, Path::new("c")).unwrap();
    let direct_mode = resolve_app_c_mode(&path, Path::new("c")).unwrap();

    assert_eq!(
        mode,
        ArceosBuildMode::AppC {
            app_dir: app_dir.clone(),
            app_name: "case".to_string()
        }
    );
    assert_eq!(app_dir, source_dir.canonicalize().unwrap());
    assert_eq!(direct_mode, mode);
}

#[test]
fn app_c_build_config_rejects_missing_source_dir() {
    let root = tempdir().unwrap();
    let path = root.path().join("build-x86_64-unknown-none.toml");
    fs::write(
        &path,
        "app-c = \"missing\"\nfeatures = []\nlog = \"Warn\"\n",
    )
    .unwrap();

    let err = load_arceos_build_mode(&path).unwrap_err();

    assert!(
        err.to_string().contains("app-c source directory"),
        "{err:#}"
    );
}

#[test]
fn app_c_build_config_rejects_source_dir_without_c_files() {
    let root = tempdir().unwrap();
    let source_dir = root.path().join("c");
    fs::create_dir_all(&source_dir).unwrap();
    fs::write(source_dir.join("main.rs"), "fn main() {}\n").unwrap();
    let path = root.path().join("build-x86_64-unknown-none.toml");
    fs::write(&path, "app-c = \"c\"\nfeatures = []\nlog = \"Warn\"\n").unwrap();

    let err = load_arceos_build_mode(&path).unwrap_err();

    assert!(
        err.to_string()
            .contains("must contain at least one direct .c file"),
        "{err:#}"
    );
}

#[test]
fn load_build_info_rejects_legacy_feature_aliases() {
    let root = tempdir().unwrap();
    let path = root.path().join(".build-target.toml");
    fs::write(
        &path,
        r#"
features = ["axstd", "ax-std/smp", "ax-runtime/net"]
log = "Warn"

"#,
    )
    .unwrap();
    let request = request("arceos-helloworld", "target", path.clone());

    let err = load_build_info(&request).unwrap_err();

    assert!(err.to_string().contains("removed `axstd` alias"));
}

#[test]
fn load_build_info_defaults_unspecified_aarch64_to_dynamic_platform() {
    let root = tempdir().unwrap();
    let path = root
        .path()
        .join("build-aarch64-unknown-none-softfloat.toml");
    fs::write(
        &path,
        r#"
features = ["ax-std", "ax-std/backtrace"]
log = "Info"

[env]
BACKTRACE = "y"
"#,
    )
    .unwrap();
    let request = request(
        "arceos-test-suit",
        "aarch64-unknown-none-softfloat",
        path.clone(),
    );

    let build_info = load_build_info(&request).unwrap();

    let metadata = repo_metadata();
    let cargo = build_info
        .into_prepared_base_cargo_config_with_metadata(&request.package, &request.target, &metadata)
        .unwrap();

    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json")
    );
    assert!(!cargo.env.contains_key("AX_CONFIG_PATH"));
}

#[test]
fn load_build_info_defaults_unspecified_riscv_to_dynamic_platform() {
    let root = tempdir().unwrap();
    let path = root.path().join("build-riscv64gc-unknown-none-elf.toml");
    fs::write(
        &path,
        r#"
features = ["ax-std"]
log = "Warn"
max_cpu_num = 4

"#,
    )
    .unwrap();
    let request = request("arceos-test-suit", "riscv64gc-unknown-none-elf", path);

    let build_info = load_build_info(&request).unwrap();

    let metadata = repo_metadata();
    let cargo = build_info
        .into_prepared_base_cargo_config_with_metadata(&request.package, &request.target, &metadata)
        .unwrap();

    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(
        cargo
            .features
            .iter()
            .all(|feature| !feature.starts_with("ax-std/riscv64-"))
    );
    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/riscv64gc-unknown-linux-musl.json")
    );
}

#[test]
fn load_build_info_defaults_unspecified_loongarch64_to_dynamic_platform() {
    let root = tempdir().unwrap();
    let path = root
        .path()
        .join("build-loongarch64-unknown-none-softfloat.toml");
    fs::write(
        &path,
        r#"
features = ["ax-std"]
log = "Warn"

"#,
    )
    .unwrap();
    let request = request(
        "arceos-test-suit",
        "loongarch64-unknown-none-softfloat",
        path,
    );

    let build_info = load_build_info(&request).unwrap();

    let metadata = repo_metadata();
    let cargo = build_info
        .into_prepared_base_cargo_config_with_metadata(&request.package, &request.target, &metadata)
        .unwrap();

    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/loongarch64-unknown-linux-musl.json")
    );
}

#[test]
fn parse_makefile_features_splits_commas_whitespace_and_dedups() {
    assert_eq!(
        build::parse_makefile_features(" lockdep, sched-rr  lockdep\tax-runtime/net "),
        vec![
            "lockdep".to_string(),
            "sched-rr".to_string(),
            "ax-runtime/net".to_string()
        ]
    );
}

#[test]
fn apply_makefile_features_uses_ax_std_prefix_for_unified_std_build() {
    let mut build_info = ArceosBuildInfo {
        features: Vec::new(),
        ..ArceosBuildInfo::default()
    };

    build::apply_makefile_features(&mut build_info, &[String::from("lockdep")]).unwrap();

    assert!(build_info.features.contains(&"lockdep".to_string()));
    assert!(!build_info.features.contains(&"ax-api/lockdep".to_string()));
}

#[test]
fn prepared_cargo_config_uses_unified_std_target() {
    let metadata = repo_metadata();
    let cargo = ArceosBuildInfo {
        features: vec!["lockdep".to_string()],
        ..ArceosBuildInfo::default()
    }
    .into_prepared_base_cargo_config_with_metadata(
        "arceos-helloworld",
        "aarch64-unknown-none-softfloat",
        &metadata,
    )
    .unwrap();

    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json")
    );
    assert!(cargo.features.contains(&"ax-std/lockdep".to_string()));
}

#[test]
fn c_app_cargo_config_uses_builtin_bare_target_without_json_spec() {
    let root = tempdir().unwrap();
    let build_config = root
        .path()
        .join("build-loongarch64-unknown-none-softfloat.toml");
    let build_info = ArceosBuildInfo {
        features: vec!["ax-std".to_string()],
        ..ArceosBuildInfo::default()
    };
    fs::write(&build_config, toml::to_string_pretty(&build_info).unwrap()).unwrap();
    let request = request(
        "arceos-helloworld",
        "loongarch64-unknown-none-softfloat",
        build_config,
    );
    let cargo = load_c_app_cargo_config(&request).unwrap();

    assert_eq!(cargo.target, "loongarch64-unknown-none-softfloat");
    assert!(!cargo.env.contains_key("CARGO_UNSTABLE_JSON_TARGET_SPEC"));
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(
        cargo
            .args
            .windows(2)
            .any(|pair| pair == ["-Z", "build-std=core,alloc"])
    );
}

#[test]
fn to_cargo_config_maps_max_cpu_num_to_smp_env_for_dynamic_platforms() {
    let root = tempdir().unwrap();
    let request = request(
        "arceos-helloworld",
        "aarch64-unknown-none-softfloat",
        root.path().join(".build.toml"),
    );

    let metadata = repo_metadata();
    let cargo = ArceosBuildInfo {
        max_cpu_num: Some(4),
        ..ArceosBuildInfo::default()
    }
    .into_prepared_base_cargo_config_with_metadata(&request.package, &request.target, &metadata)
    .unwrap();

    assert_eq!(cargo.env.get("SMP"), Some(&"4".to_string()));
    assert!(cargo.features.contains(&"ax-std/smp".to_string()));
}

#[test]
fn prepared_cargo_config_defaults_x86_64_to_dynamic_platform() {
    let metadata = repo_metadata();
    let cargo = ArceosBuildInfo::default()
        .into_prepared_base_cargo_config_with_metadata(
            "arceos-helloworld",
            "x86_64-unknown-none",
            &metadata,
        )
        .unwrap();

    assert!(!cargo.to_bin);
    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/x86_64-unknown-linux-musl.json")
    );
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-hal/x86-pc".to_string()));
}
