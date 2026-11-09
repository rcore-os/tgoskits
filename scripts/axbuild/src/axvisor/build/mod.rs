mod features;
mod load;
mod metadata;
mod vm_config;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

pub use ax_config::CommonConfig;

pub use super::config::AxvisorConfig;
pub const AXVISOR_PACKAGE: &str = "axvisor";
pub(crate) use load::{
    default_build_info_path, load_board_file, load_or_create_default_config,
    resolve_build_info_path,
};
use ostool::build::config::Cargo;

use self::{
    features::reject_unsupported_nested_platform_features, metadata::platform_feature_names,
};
pub use crate::build::LogLevel;
use crate::context::{ResolvedAxvisorRequest, WorkspaceContext};

pub(crate) fn load_cargo_config(
    request: &ResolvedAxvisorRequest,
    workspace: &WorkspaceContext,
) -> anyhow::Result<Cargo> {
    load_cargo_config_with_makefile_features(request, workspace, &[])
}

fn load_cargo_config_with_makefile_features(
    request: &ResolvedAxvisorRequest,
    workspace: &WorkspaceContext,
    makefile_features: &[String],
) -> anyhow::Result<Cargo> {
    to_cargo_config(
        request.config.clone(),
        request,
        workspace,
        makefile_features,
    )
}

fn to_cargo_config(
    mut config: AxvisorConfig,
    request: &ResolvedAxvisorRequest,
    workspace: &WorkspaceContext,
    makefile_features: &[String],
) -> anyhow::Result<Cargo> {
    config.validate()?;
    crate::build::apply_makefile_features(&mut config.common, makefile_features)?;
    let known_platforms = platform_feature_names(workspace.metadata());
    reject_unsupported_nested_platform_features(&config.common.features, &known_platforms)?;
    let mut cargo = crate::build::into_prepared_std_cargo_config_with_metadata(
        config.common,
        &request.package,
        &request.target,
        workspace.metadata(),
        &workspace.axbuild_artifact_dir(),
        request.runtime,
    )?;
    patch_axvisor_cargo_config(&mut cargo, request);
    Ok(cargo)
}

fn patch_axvisor_cargo_config(cargo: &mut Cargo, request: &ResolvedAxvisorRequest) {
    cargo.package = request.package.clone();
    ensure_axvisor_bin_arg(&mut cargo.args);
    cargo
        .env
        .insert("AX_ARCH".to_string(), request.arch.clone());
    cargo
        .env
        .insert("AX_TARGET".to_string(), request.target.clone());
    if request.arch == "x86_64" {
        // The x86_64 std target disables every vector ISA (kernel policy: host
        // task switches do not save XMM state). httparse otherwise compiles its
        // SSE4.2 module for any std x86_64 build and LLVM cannot legalize the
        // 128-bit vectors, so take the crate's documented no-SIMD switch. The
        // build script only re-reads this on a fresh run; flipping it requires
        // `cargo clean -p httparse` once on machines with cached artifacts.
        cargo.env.insert(
            "CARGO_CFG_HTTPARSE_DISABLE_SIMD".to_string(),
            "1".to_string(),
        );
    }
    cargo.features.sort();
    cargo.features.dedup();
}

/// Resolves bundle inputs independently of Cargo's compilation identity.
pub(crate) fn load_vmconfigs(
    request: &ResolvedAxvisorRequest,
    workspace: &WorkspaceContext,
) -> anyhow::Result<Vec<PathBuf>> {
    let config = &request.config;
    let paths = if request.vmconfigs.is_empty() {
        config
            .vm_configs
            .iter()
            .map(|path| resolve_build_config_vmconfig_path(request, path))
            .collect()
    } else {
        request.vmconfigs.clone()
    };
    resolve_vmconfigs(request, &paths, workspace)
}

pub(crate) fn resolve_vmconfigs(
    request: &ResolvedAxvisorRequest,
    paths: &[PathBuf],
    workspace: &WorkspaceContext,
) -> anyhow::Result<Vec<PathBuf>> {
    vm_config::resolve_vmconfigs(request, paths, workspace)
}

fn resolve_build_config_vmconfig_path(request: &ResolvedAxvisorRequest, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let workspace_root = request
        .axvisor_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&request.axvisor_dir);
    workspace_root.join(path)
}

fn ensure_axvisor_bin_arg(args: &mut Vec<String>) {
    if args.iter().any(|arg| arg == "--bin") {
        return;
    }

    args.push("--bin".to_string());
    args.push(AXVISOR_PACKAGE.to_string());
}
