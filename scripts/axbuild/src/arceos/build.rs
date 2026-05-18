use std::{fs, path::PathBuf};

use anyhow::Context;
use cargo_metadata::Metadata;
use log::warn;
use ostool::build::config::Cargo;
pub use ostool::build::config::LogLevel;

use crate::{
    build::{self, BuildInfo},
    context::ResolvedBuildRequest,
};

pub type ArceosBuildInfo = BuildInfo;

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

fn load_build_info_with_makefile_features_and_metadata(
    request: &ResolvedBuildRequest,
    makefile_features: &[String],
    metadata: Option<&Metadata>,
) -> anyhow::Result<ArceosBuildInfo> {
    build::ensure_build_info(&request.build_info_path, || {
        ArceosBuildInfo::default_for_target(&request.target)
    })?;
    let mut build_info: ArceosBuildInfo = build::load_build_info(&request.build_info_path)?;

    if build_info.normalize_legacy_feature_aliases() {
        warn!(
            "normalizing legacy feature aliases in build config {}",
            request.build_info_path.display()
        );
        fs::write(
            &request.build_info_path,
            toml::to_string_pretty(&build_info)?,
        )
        .with_context(|| {
            format!(
                "failed to rewrite normalized build info {}",
                request.build_info_path.display()
            )
        })?;
    }

    match metadata {
        Some(metadata) => build::apply_makefile_features_with_metadata(
            &mut build_info,
            &request.package,
            makefile_features,
            metadata,
        ),
        None => {
            build::apply_makefile_features(&mut build_info, &request.package, makefile_features)
        }
    }

    if let Some(smp) = request.smp {
        build_info.max_cpu_num = Some(smp);
    }

    Ok(build_info)
}

pub(crate) fn load_cargo_config(request: &ResolvedBuildRequest) -> anyhow::Result<Cargo> {
    let metadata =
        build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    let makefile_features = build::makefile_features_from_env();
    let build_info = load_build_info_with_makefile_features_and_metadata(
        request,
        &makefile_features,
        Some(metadata),
    )?;

    build_info.into_prepared_base_cargo_config_with_metadata(
        &request.package,
        &request.target,
        request.plat_dyn,
        metadata,
    )
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

    use ostool::build;
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
        build_info.resolve_features("ax-helloworld", true);

        assert!(build_info.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(!build_info.features.contains(&"ax-std/defplat".to_string()));

        let args = ArceosBuildInfo::build_cargo_args("aarch64-unknown-none-softfloat", true, &[]);
        assert!(args.iter().any(|arg| arg.contains("-Taxplat.x")));
    }

    #[test]
    fn resolves_non_dynamic_platform_features_and_args() {
        let mut build_info = ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat");
        build_info.resolve_features("ax-helloworld", false);

        assert!(build_info.features.contains(&"ax-std/defplat".to_string()));
        assert!(!build_info.features.contains(&"ax-std/plat-dyn".to_string()));

        let args = ArceosBuildInfo::build_cargo_args("aarch64-unknown-none-softfloat", false, &[]);
        assert!(args.iter().any(|arg| arg.contains("-Tlinker.x")));
    }

    #[test]
    fn max_cpu_num_adds_axfeat_smp_feature() {
        let metadata = repo_metadata();
        let mut build_info = ArceosBuildInfo {
            features: vec!["ax-feat/net".to_string()],
            max_cpu_num: Some(4),
            ..ArceosBuildInfo::default()
        };

        build_info.resolve_features_with_metadata("starryos", false, &metadata);

        assert!(build_info.features.contains(&"ax-feat/smp".to_string()));
    }

    #[test]
    fn resolve_build_info_path_uses_package_directory() {
        let path = resolve_build_info_path("ax-helloworld", "aarch64-unknown-none-softfloat", None)
            .unwrap();

        assert!(path.ends_with(
            "tmp/axbuild/config/ax-helloworld/build-aarch64-unknown-none-softfloat.toml"
        ));
    }

    #[test]
    fn resolve_build_info_path_prefers_explicit_path() {
        let path = resolve_build_info_path(
            "ax-helloworld",
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
        let request = request("ax-helloworld", "target", None, path.clone());

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
        let request = request("ax-helloworld", "target", None, path.clone());

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
    fn apply_makefile_features_uses_axfeat_prefix_for_axfeat_packages() {
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

        assert!(build_info.features.contains(&"ax-feat/lockdep".to_string()));
        assert!(!build_info.features.contains(&"ax-std/lockdep".to_string()));
    }

    #[test]
    fn to_cargo_config_maps_max_cpu_num_to_smp_env_for_dynamic_platforms() {
        let root = tempdir().unwrap();
        let request = request(
            "ax-helloworld",
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
    fn base_cargo_config_defaults_to_bin_false_for_x86_64_targets() {
        let cargo = ArceosBuildInfo::default_for_target("x86_64-unknown-none")
            .into_base_cargo_config_with_log(
                "ax-helloworld".to_string(),
                "x86_64-unknown-none".to_string(),
                vec![],
            );

        assert!(!cargo.to_bin);
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
        assert!(!build::resolve_effective_plat_dyn(
            "x86_64-unknown-none",
            true,
            Some(true)
        ));
    }
}
