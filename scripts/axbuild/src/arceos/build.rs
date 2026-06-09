use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use cargo_metadata::Metadata;
use log::warn;
use ostool::build::config::Cargo;
pub use ostool::build::config::LogLevel;
use serde::{Deserialize, Serialize};

use crate::{
    build::{self, BuildInfo},
    context::ResolvedBuildRequest,
};

pub type ArceosBuildInfo = BuildInfo;

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct ArceosBuildConfig {
    #[serde(flatten, default)]
    pub(crate) build_info: ArceosBuildInfo,
    #[serde(rename = "app-c", skip_serializing_if = "Option::is_none")]
    pub(crate) app_c: Option<PathBuf>,
}

impl ArceosBuildConfig {
    fn default_for_target(target: &str) -> Self {
        Self {
            build_info: ArceosBuildInfo::default_for_target(target),
            app_c: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArceosBuildMode {
    RustStd,
    AppC { app_dir: PathBuf, app_name: String },
}

pub(crate) fn resolve_build_info_path(
    package: &str,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }

    default_build_info_path(package, target)
}

#[cfg(test)]
fn load_build_info(request: &ResolvedBuildRequest) -> anyhow::Result<ArceosBuildInfo> {
    let makefile_features = build::makefile_features_from_env();
    load_build_info_with_makefile_features(request, &makefile_features)
}

#[cfg(test)]
fn load_build_info_with_makefile_features(
    request: &ResolvedBuildRequest,
    makefile_features: &[String],
) -> anyhow::Result<ArceosBuildInfo> {
    let metadata = if makefile_features.is_empty() {
        None
    } else {
        Some(build::workspace_metadata().context("failed to load workspace metadata")?)
    };
    load_build_info_with_makefile_features_and_metadata(
        request,
        makefile_features,
        metadata.as_ref(),
    )
}

#[cfg(test)]
fn load_build_info_with_makefile_features_and_metadata(
    request: &ResolvedBuildRequest,
    makefile_features: &[String],
    metadata: Option<&Metadata>,
) -> anyhow::Result<ArceosBuildInfo> {
    Ok(
        load_build_config_with_makefile_features_and_metadata(
            request,
            makefile_features,
            metadata,
        )?
        .build_info,
    )
}

fn load_build_config_with_makefile_features_and_metadata(
    request: &ResolvedBuildRequest,
    makefile_features: &[String],
    metadata: Option<&Metadata>,
) -> anyhow::Result<ArceosBuildConfig> {
    build::ensure_build_info(&request.build_info_path, || {
        ArceosBuildConfig::default_for_target(&request.target)
    })?;
    let content = fs::read_to_string(&request.build_info_path)?;
    build::reject_removed_std_field(&request.build_info_path, &content)?;
    let mut config: ArceosBuildConfig = toml::from_str(&content).with_context(|| {
        format!(
            "failed to parse build info {}",
            request.build_info_path.display()
        )
    })?;
    build::apply_target_defaults_if_plat_dyn_unspecified(
        &mut config.build_info,
        &request.target,
        &content,
    );

    if config.build_info.normalize_legacy_feature_aliases() {
        warn!(
            "normalizing legacy feature aliases in build config {}",
            request.build_info_path.display()
        );
        fs::write(&request.build_info_path, toml::to_string_pretty(&config)?).with_context(
            || {
                format!(
                    "failed to rewrite normalized build info {}",
                    request.build_info_path.display()
                )
            },
        )?;
    }

    match metadata {
        Some(metadata) => build::apply_makefile_features_with_metadata(
            &mut config.build_info,
            &request.package,
            makefile_features,
            metadata,
        ),
        None => build::apply_makefile_features(
            &mut config.build_info,
            &request.package,
            makefile_features,
        ),
    }

    if let Some(smp) = request.smp {
        config.build_info.max_cpu_num = Some(smp);
    }

    Ok(config)
}

pub(crate) fn load_cargo_config(request: &ResolvedBuildRequest) -> anyhow::Result<Cargo> {
    let metadata =
        build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    let makefile_features = build::makefile_features_from_env();
    let config = load_build_config_with_makefile_features_and_metadata(
        request,
        &makefile_features,
        Some(metadata),
    )?;
    if config.app_c.is_some() {
        bail!(
            "ArceOS build config {} uses `app-c`; use the C app build path",
            request.build_info_path.display()
        );
    }
    let build_info = config.build_info;

    build_info.into_prepared_base_cargo_config_with_metadata(
        &request.package,
        &request.target,
        request.plat_dyn,
        metadata,
    )
}

pub(crate) fn load_c_app_cargo_config(request: &ResolvedBuildRequest) -> anyhow::Result<Cargo> {
    let metadata =
        build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    let makefile_features = build::makefile_features_from_env();
    let mut build_info = load_build_config_with_makefile_features_and_metadata(
        request,
        &makefile_features,
        Some(metadata),
    )?
    .build_info;
    let plat_dyn = build_info.effective_plat_dyn(&request.target, request.plat_dyn);

    build_info.validated_max_cpu_num()?;
    build_info.prepare_non_dynamic_platform_for(
        &request.package,
        &request.target,
        plat_dyn,
        metadata,
    )?;
    build_info.resolve_features_with_metadata(
        &request.package,
        &request.target,
        plat_dyn,
        metadata,
    );
    let rustflags = build::toolchain_rustflags(&build_info.env);
    let args = ArceosBuildInfo::build_cargo_args(&request.target, &rustflags);

    build_info.prepare_log_env();
    build_info.prepare_max_cpu_num_env()?;

    Ok(build_info.into_base_cargo_config_with_to_bin(
        request.package.clone(),
        request.target.clone(),
        args,
        false,
    ))
}

pub(crate) fn load_arceos_build_config(path: &Path) -> anyhow::Result<ArceosBuildConfig> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read ArceOS build config {}", path.display()))?;
    build::reject_removed_std_field(path, &content)?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse ArceOS build config {}", path.display()))
}

pub(crate) fn load_arceos_build_mode(path: &Path) -> anyhow::Result<ArceosBuildMode> {
    let config = load_arceos_build_config(path)?;
    match config.app_c {
        Some(app_c) => resolve_app_c_mode(path, &app_c),
        None => Ok(ArceosBuildMode::RustStd),
    }
}

pub(crate) fn resolve_app_c_mode(
    config_path: &Path,
    app_c: &Path,
) -> anyhow::Result<ArceosBuildMode> {
    let app_dir = resolve_app_c_dir(config_path, app_c)?;
    let app_name = c_app_name(&app_dir)
        .with_context(|| format!("failed to derive C app name from {}", app_dir.display()))?;

    Ok(ArceosBuildMode::AppC { app_dir, app_name })
}

pub(crate) fn resolve_app_c_dir(config_path: &Path, app_c: &Path) -> anyhow::Result<PathBuf> {
    let app_dir = if app_c.is_absolute() {
        app_c.to_path_buf()
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(app_c)
    };

    if !app_dir.is_dir() {
        bail!(
            "app-c source directory {} configured by {} does not exist or is not a directory",
            app_dir.display(),
            config_path.display()
        );
    }
    if !dir_has_direct_c_source(&app_dir)? {
        bail!(
            "app-c source directory {} configured by {} must contain at least one direct .c file",
            app_dir.display(),
            config_path.display()
        );
    }

    app_dir.canonicalize().with_context(|| {
        format!(
            "failed to resolve app-c source directory {}",
            app_dir.display()
        )
    })
}

fn dir_has_direct_c_source(dir: &Path) -> anyhow::Result<bool> {
    Ok(fs::read_dir(dir)
        .with_context(|| format!("failed to read app-c source directory {}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "c")))
}

fn c_app_name(app_dir: &Path) -> Option<String> {
    let name_dir = if app_dir.file_name().and_then(|name| name.to_str()) == Some("c") {
        app_dir.parent().unwrap_or(app_dir)
    } else {
        app_dir
    };

    name_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
}

pub(crate) fn default_build_info_path(package: &str, target: &str) -> anyhow::Result<PathBuf> {
    Ok(build::default_build_info_path_in_workspace(
        &crate::context::workspace_root_path()?,
        package,
        target,
    ))
}

#[cfg(test)]
fn resolve_build_info_path_in_dir(dir: &std::path::Path, target: &str) -> PathBuf {
    let bare_path = dir.join(format!("build-{target}.toml"));
    if bare_path.exists() {
        return bare_path;
    }

    let dotted_path = dir.join(format!(".build-{target}.toml"));
    if dotted_path.exists() {
        return dotted_path;
    }

    dotted_path
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn repo_metadata() -> cargo_metadata::Metadata {
        build::workspace_metadata().unwrap()
    }

    fn request(
        package: &str,
        target: &str,
        plat_dyn: Option<bool>,
        build_info_path: PathBuf,
    ) -> ResolvedBuildRequest {
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
            plat_dyn,
            smp: None,
            debug: false,
            build_info_path,
            qemu_config: None,
            uboot_config: None,
        }
    }

    #[test]
    fn resolves_dynamic_platform_features_and_args() {
        let mut build_info = ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat");
        build_info.resolve_features(
            "arceos-std-helloworld",
            "aarch64-unknown-none-softfloat",
            true,
        );

        assert!(build_info.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(!build_info.features.contains(&"ax-hal/plat-dyn".to_string()));
        assert!(!build_info.features.contains(&"ax-std/defplat".to_string()));

        let args = ArceosBuildInfo::build_cargo_args("aarch64-unknown-none-softfloat", &[]);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-Z", "build-std=core,alloc"])
        );
        assert!(!args.iter().any(|arg| arg.contains("-Clink-arg=-T")));
    }

    #[test]
    fn resolves_non_dynamic_aarch64_to_defplat_without_static_default() {
        let mut build_info = ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat");
        build_info.resolve_features(
            "arceos-std-helloworld",
            "aarch64-unknown-none-softfloat",
            false,
        );

        assert!(build_info.features.contains(&"ax-hal/defplat".to_string()));
        assert!(
            !build_info
                .features
                .contains(&"ax-hal/aarch64-qemu-virt".to_string())
        );
        assert!(!build_info.features.contains(&"ax-std/plat-dyn".to_string()));

        let args = ArceosBuildInfo::build_cargo_args("aarch64-unknown-none-softfloat", &[]);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-Z", "build-std=core,alloc"])
        );
        assert!(!args.iter().any(|arg| arg.contains("-Clink-arg=-T")));
    }

    #[test]
    fn preparing_c_app_non_dynamic_aarch64_without_custom_platform_fails() {
        let metadata = repo_metadata();
        let mut build_info = ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat");
        let result = build_info.prepare_non_dynamic_platform_for(
            "arceos-std-helloworld",
            "aarch64-unknown-none-softfloat",
            false,
            &metadata,
        );

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no default platform package is registered for arch `aarch64`")
        );
    }

    #[test]
    fn max_cpu_num_adds_smp_feature_for_std_build() {
        let metadata = repo_metadata();
        let mut build_info = ArceosBuildInfo {
            features: vec!["ax-feat/net".to_string()],
            max_cpu_num: Some(4),
            ..ArceosBuildInfo::default()
        };

        build_info.resolve_features_with_metadata(
            "starryos",
            "aarch64-unknown-none-softfloat",
            false,
            &metadata,
        );

        assert!(build_info.features.contains(&"ax-std/smp".to_string()));
    }

    #[test]
    fn resolve_build_info_path_uses_package_directory() {
        let path = resolve_build_info_path(
            "arceos-std-helloworld",
            "aarch64-unknown-none-softfloat",
            None,
        )
        .unwrap();

        assert!(path.ends_with(
            "tmp/axbuild/config/arceos-std-helloworld/build-aarch64-unknown-none-softfloat.toml"
        ));
    }

    #[test]
    fn resolve_build_info_path_prefers_explicit_path() {
        let path = resolve_build_info_path(
            "arceos-std-helloworld",
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
        let request = request("arceos-std-helloworld", "target", None, path.clone());

        let build_info = load_build_info(&request).unwrap();

        assert_eq!(build_info, ArceosBuildInfo::default_for_target("target"));
        assert!(path.exists());
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("features = [\"ax-std\"]")
        );
    }

    #[test]
    fn build_config_without_app_c_uses_std_rust_mode() {
        let root = tempdir().unwrap();
        let path = root.path().join("build-x86_64-unknown-none.toml");
        fs::write(
            &path,
            "features = [\"ax-std\"]\nlog = \"Warn\"\n\n[env]\nAX_IP = \"10.0.2.15\"\n",
        )
        .unwrap();

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
        fs::write(
            &path,
            "app-c = \"c\"\nfeatures = []\nlog = \"Warn\"\n\n[env]\nAX_IP = \"10.0.2.15\"\n",
        )
        .unwrap();

        let mode = load_arceos_build_mode(&path).unwrap();

        assert_eq!(
            mode,
            ArceosBuildMode::AppC {
                app_dir: source_dir.canonicalize().unwrap(),
                app_name: "case".to_string()
            }
        );
    }

    #[test]
    fn app_c_build_config_rejects_missing_source_dir() {
        let root = tempdir().unwrap();
        let path = root.path().join("build-x86_64-unknown-none.toml");
        fs::write(
            &path,
            "app-c = \"missing\"\nfeatures = []\nlog = \"Warn\"\n\n[env]\nAX_IP = \"10.0.2.15\"\n",
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
        fs::write(
            &path,
            "app-c = \"c\"\nfeatures = []\nlog = \"Warn\"\n\n[env]\nAX_IP = \"10.0.2.15\"\n",
        )
        .unwrap();

        let err = load_arceos_build_mode(&path).unwrap_err();

        assert!(
            err.to_string()
                .contains("must contain at least one direct .c file"),
            "{err:#}"
        );
    }

    #[test]
    fn load_build_info_normalizes_legacy_feature_aliases() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        fs::write(
            &path,
            r#"
features = ["axstd", "axstd/smp", "axfeat/net"]
log = "Warn"

[env]
AX_IP = "10.0.2.15"
"#,
        )
        .unwrap();
        let request = request("arceos-std-helloworld", "target", None, path.clone());

        let build_info = load_build_info(&request).unwrap();

        assert!(build_info.features.contains(&"ax-std".to_string()));
        assert!(build_info.features.contains(&"ax-std/smp".to_string()));
        assert!(build_info.features.contains(&"ax-feat/net".to_string()));
        assert!(!build_info.features.contains(&"axstd".to_string()));

        let rewritten = fs::read_to_string(path).unwrap();
        assert!(rewritten.contains("ax-std"));
        assert!(!rewritten.contains("axstd"));
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
            None,
            path.clone(),
        );

        let build_info = load_build_info(&request).unwrap();

        assert!(build_info.plat_dyn);

        let metadata = repo_metadata();
        let cargo = build_info
            .into_prepared_base_cargo_config_with_metadata(
                &request.package,
                &request.target,
                request.plat_dyn,
                &metadata,
            )
            .unwrap();

        assert!(cargo.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(
            cargo
                .target
                .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json")
        );
        assert!(!cargo.env.contains_key("AX_CONFIG_PATH"));
    }

    #[test]
    fn load_build_info_preserves_explicit_non_dynamic_aarch64() {
        let root = tempdir().unwrap();
        let path = root
            .path()
            .join("build-aarch64-unknown-none-softfloat.toml");
        fs::write(
            &path,
            r#"
features = ["ax-std"]
env = {}
log = "Info"
plat_dyn = false
"#,
        )
        .unwrap();
        let request = request(
            "arceos-std-helloworld",
            "aarch64-unknown-none-softfloat",
            None,
            path,
        );

        let build_info = load_build_info(&request).unwrap();

        assert!(!build_info.plat_dyn);
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

[env]
AX_GW = "10.0.2.2"
AX_IP = "10.0.2.15"
"#,
        )
        .unwrap();
        let request = request("arceos-test-suit", "riscv64gc-unknown-none-elf", None, path);

        let build_info = load_build_info(&request).unwrap();

        assert!(build_info.plat_dyn);

        let metadata = repo_metadata();
        let cargo = build_info
            .into_prepared_base_cargo_config_with_metadata(
                &request.package,
                &request.target,
                request.plat_dyn,
                &metadata,
            )
            .unwrap();

        assert!(cargo.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(
            !cargo
                .features
                .contains(&"ax-std/riscv64-qemu-virt".to_string())
        );
        assert!(
            cargo
                .target
                .ends_with("scripts/targets/std/pie/riscv64gc-unknown-linux-musl.json")
        );
    }

    #[test]
    fn parse_makefile_features_splits_commas_whitespace_and_dedups() {
        assert_eq!(
            build::parse_makefile_features(" lockdep, sched-rr  lockdep\taxfeat/net "),
            vec![
                "lockdep".to_string(),
                "sched-rr".to_string(),
                "axfeat/net".to_string()
            ]
        );
    }

    #[test]
    fn apply_makefile_features_uses_ax_std_prefix_for_unified_std_build() {
        let metadata = repo_metadata();
        let mut build_info = ArceosBuildInfo {
            features: Vec::new(),
            ..ArceosBuildInfo::default()
        };

        build::apply_makefile_features_with_metadata(
            &mut build_info,
            "starryos",
            &[String::from("lockdep")],
            &metadata,
        );

        assert!(build_info.features.contains(&"lockdep".to_string()));
        assert!(!build_info.features.contains(&"ax-feat/lockdep".to_string()));
    }

    #[test]
    fn prepared_cargo_config_uses_unified_std_target() {
        let metadata = repo_metadata();
        let cargo = ArceosBuildInfo {
            features: vec!["lockdep".to_string()],
            ..ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat")
        }
        .into_prepared_base_cargo_config_with_metadata(
            "arceos-std-helloworld",
            "aarch64-unknown-none-softfloat",
            None,
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
        let build_config = root.path().join("build-x86_64-unknown-none.toml");
        let build_info = ArceosBuildInfo {
            features: vec!["ax-std".to_string()],
            ..ArceosBuildInfo::default_for_target("x86_64-unknown-none")
        };
        fs::write(&build_config, toml::to_string_pretty(&build_info).unwrap()).unwrap();
        let request = request(
            "arceos-std-helloworld",
            "x86_64-unknown-none",
            Some(false),
            build_config,
        );
        let cargo = load_c_app_cargo_config(&request).unwrap();

        assert_eq!(cargo.target, "x86_64-unknown-none");
        assert!(!cargo.env.contains_key("CARGO_UNSTABLE_JSON_TARGET_SPEC"));
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
            "arceos-std-helloworld",
            "aarch64-unknown-none-softfloat",
            Some(true),
            root.path().join(".build.toml"),
        );

        let metadata = repo_metadata();
        let cargo = ArceosBuildInfo {
            max_cpu_num: Some(4),
            ..ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat")
        }
        .into_prepared_base_cargo_config_with_metadata(
            &request.package,
            &request.target,
            request.plat_dyn,
            &metadata,
        )
        .unwrap();

        assert_eq!(cargo.env.get("SMP"), Some(&"4".to_string()));
        assert!(cargo.features.contains(&"ax-std/smp".to_string()));
    }

    #[test]
    fn prepared_cargo_config_defaults_x86_64_to_dynamic_platform() {
        let metadata = repo_metadata();
        let cargo = ArceosBuildInfo::default_for_target("x86_64-unknown-none")
            .into_prepared_base_cargo_config_with_metadata(
                "arceos-std-helloworld",
                "x86_64-unknown-none",
                None,
                &metadata,
            )
            .unwrap();

        assert!(cargo.to_bin);
        assert!(
            cargo
                .target
                .ends_with("scripts/targets/std/pie/x86_64-unknown-linux-musl.json")
        );
        assert!(cargo.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(!cargo.features.contains(&"ax-hal/x86-pc".to_string()));
    }

    #[test]
    fn resolve_effective_plat_dyn_uses_override_and_target_support() {
        assert!(build::resolve_effective_plat_dyn(
            "aarch64-unknown-none-softfloat",
            true,
            None
        ));
        assert!(!build::resolve_effective_plat_dyn(
            "aarch64-unknown-none-softfloat",
            true,
            Some(false)
        ));
        assert!(build::resolve_effective_plat_dyn(
            "riscv64gc-unknown-none-elf",
            true,
            None
        ));
        assert!(build::resolve_effective_plat_dyn(
            "x86_64-unknown-none",
            true,
            Some(true)
        ));
    }
}
