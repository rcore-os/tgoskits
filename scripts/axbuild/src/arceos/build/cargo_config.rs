use anyhow::{Context, bail};
use cargo_metadata::Metadata;
use ostool::build::config::Cargo;

use super::{ArceosBuildInfo, info::load_build_config_with_makefile_features_and_metadata};
use crate::{build, context::ResolvedBuildRequest};

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
        metadata,
    )
}

/// Loads Cargo options for an ArceOS guest that Axvisor direct-boots from a
/// memory image.
///
/// This path is used by test guests whose image is embedded into Axvisor via
/// `image_location = "memory"`. They still run the normal ArceOS runtime, but
/// need a direct-boot binary layout (`linker.x`, PIE relocation flags, and
/// explicit `arceos` feature selection) instead of the host/std Axvisor target
/// or the legacy x86 guest image path.
pub(crate) fn load_direct_guest_cargo_config(
    request: &ResolvedBuildRequest,
) -> anyhow::Result<Cargo> {
    let metadata =
        build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    let makefile_features = build::makefile_features_from_env();
    let mut build_info = load_build_config_with_makefile_features_and_metadata(
        request,
        &makefile_features,
        Some(metadata),
    )?
    .build_info;
    build_info.validated_max_cpu_num()?;
    build_info.resolve_features_with_metadata(&request.package, &request.target, metadata);
    remove_undeclared_package_features(&mut build_info.features, &request.package, metadata);
    enable_arceos_feature_if_declared(&mut build_info.features, &request.package, metadata);
    let mut rustflags =
        build::toolchain_rustflags_for_features(&build_info.env, &build_info.features);
    rustflags.extend(
        arceos_direct_guest_link_rustflags()
            .into_iter()
            .map(str::to_string),
    );
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
    build_info.validated_max_cpu_num()?;
    build_info.resolve_features_with_metadata(&request.package, &request.target, metadata);
    let rustflags = build::toolchain_rustflags_for_features(&build_info.env, &build_info.features);
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

fn arceos_direct_guest_link_rustflags() -> [&'static str; 4] {
    [
        "-Crelocation-model=pic",
        "-Clink-arg=-pie",
        "-Clink-arg=-znostart-stop-gc",
        "-Clink-arg=-Tlinker.x",
    ]
}

fn remove_undeclared_package_features(
    features: &mut Vec<String>,
    package: &str,
    metadata: &Metadata,
) {
    let Some(package_features) = metadata
        .packages
        .iter()
        .find(|pkg| pkg.name == package)
        .map(|pkg| &pkg.features)
    else {
        return;
    };

    features.retain(|feature| feature.contains('/') || package_features.contains_key(feature));
}

fn enable_arceos_feature_if_declared(
    features: &mut Vec<String>,
    package: &str,
    metadata: &Metadata,
) {
    let declares_arceos = metadata
        .packages
        .iter()
        .find(|pkg| pkg.name == package)
        .is_some_and(|pkg| pkg.features.contains_key("arceos"));
    if declares_arceos && !features.iter().any(|feature| feature == "arceos") {
        features.push("arceos".to_string());
        features.sort();
        features.dedup();
    }
}
