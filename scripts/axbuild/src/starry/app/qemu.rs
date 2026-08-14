use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use serde::Deserialize;

use super::{
    StarryAppQemuCase,
    build_config::{collect_prefixed_toml_files, discover_optional_build_config},
    discovery::resolve_case_relative_path,
    rootfs::prepare_qemu_app_rootfs,
    types::{
        AppOwnedRootfsPreparation, RootfsPreparation, RootfsPreparationConfig,
        RootfsPreparationMode, StarryAppCase, StarryAppKind,
    },
};
use crate::{
    context::{DEFAULT_STARRY_ARCH, starry_target_for_arch_checked},
    test::{
        case::TestQemuCase,
        qemu::{self as qemu_test},
    },
};

#[derive(Debug)]
struct LoadedQemuAppCaseFields {
    test_case: TestQemuCase,
    rootfs_path: Option<PathBuf>,
    rootfs_preparation: RootfsPreparation,
    write_policy: crate::rootfs::qemu::RootfsWritePolicy,
}

#[derive(Debug, Default, Deserialize)]
struct QemuAppConfig {
    #[serde(default)]
    rootfs_preparation: RootfsPreparationConfig,
}

pub(crate) async fn prepare_qemu_app_case(
    workspace_root: &Path,
    app: &StarryAppCase,
    arch: Option<&str>,
    explicit_qemu_config: Option<&Path>,
) -> anyhow::Result<StarryAppQemuCase> {
    ensure!(
        app.kind == StarryAppKind::Qemu,
        "Starry app `{}` is not a QEMU app",
        app.name
    );
    let qemu_config_path = resolve_qemu_config(app, arch, explicit_qemu_config)?;
    let arch = arch
        .map(str::to_string)
        .or_else(|| {
            qemu_config_path
                .as_deref()
                .and_then(arch_from_qemu_config_path)
                .map(str::to_string)
        })
        .unwrap_or_else(|| DEFAULT_STARRY_ARCH.to_string());
    let target = starry_target_for_arch_checked(&arch)?.to_string();
    let build_config_path = discover_optional_build_config(&app.case_dir, &target)?;
    let fields = qemu_config_path
        .as_deref()
        .map(|path| load_qemu_app_case_fields(workspace_root, app, path))
        .transpose()?;
    let rootfs_path = prepare_qemu_app_rootfs(
        workspace_root,
        app,
        &arch,
        &target,
        fields
            .as_ref()
            .and_then(|fields| fields.rootfs_path.as_deref()),
        fields
            .as_ref()
            .map(|fields| &fields.rootfs_preparation)
            .unwrap_or(&RootfsPreparation::Default),
    )
    .await?;

    Ok(StarryAppQemuCase {
        name: app.name.clone(),
        arch,
        target,
        build_config_path,
        qemu_config_path,
        rootfs_path,
        rootfs_write_policy: fields
            .as_ref()
            .map(|fields| fields.write_policy)
            .unwrap_or_default(),
        test_commands: fields
            .as_ref()
            .map(|fields| fields.test_case.test_commands.clone())
            .unwrap_or_default(),
        host_symbolize_success_regex: fields
            .as_ref()
            .map(|fields| fields.test_case.host_symbolize_success_regex.clone())
            .unwrap_or_default(),
        host_http_server: fields
            .as_ref()
            .and_then(|fields| fields.test_case.host_http_server.clone()),
        subcases: fields
            .map(|fields| fields.test_case.subcases)
            .unwrap_or_default(),
    })
}

pub(crate) fn app_qemu_test_case(
    case: &StarryAppQemuCase,
    case_dir: PathBuf,
) -> Option<TestQemuCase> {
    let qemu_config_path = case.qemu_config_path.clone()?;
    Some(TestQemuCase {
        name: case.name.clone(),
        display_name: case.name.clone(),
        case_dir,
        qemu_config_path,
        test_commands: case.test_commands.clone(),
        host_symbolize_success_regex: case.host_symbolize_success_regex.clone(),
        host_http_server: case.host_http_server.clone(),
        subcases: case.subcases.clone(),
        grouped_subcase_filter: None,
    })
}

fn load_qemu_app_case_fields(
    workspace_root: &Path,
    app: &StarryAppCase,
    qemu_config_path: &Path,
) -> anyhow::Result<LoadedQemuAppCaseFields> {
    let (test_case, write_policy) = qemu_test::load_qemu_case_fields_with_write_policy(
        app.name.clone(),
        app.name.clone(),
        app.case_dir.clone(),
        qemu_config_path.to_path_buf(),
        "Starry app",
        true,
    )?;
    let rootfs_path = qemu_app_config_rootfs_path(workspace_root, qemu_config_path)?;
    let rootfs_preparation = qemu_app_rootfs_preparation(app, qemu_config_path)?;

    Ok(LoadedQemuAppCaseFields {
        test_case,
        rootfs_path,
        rootfs_preparation,
        write_policy,
    })
}

fn qemu_app_rootfs_preparation(
    app: &StarryAppCase,
    qemu_config_path: &Path,
) -> anyhow::Result<RootfsPreparation> {
    let content = fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    let config = toml::from_str::<QemuAppConfig>(&content)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))?
        .rootfs_preparation;

    match config.mode {
        RootfsPreparationMode::Default => {
            ensure!(
                config.builder.is_none() && config.target_arch.is_none(),
                "default rootfs preparation in {} must not declare a builder or target_arch",
                qemu_config_path.display()
            );
            Ok(RootfsPreparation::Default)
        }
        RootfsPreparationMode::AppOwned => {
            let builder = config.builder.with_context(|| {
                format!(
                    "app-owned rootfs preparation in {} requires `builder`",
                    qemu_config_path.display()
                )
            })?;
            ensure!(
                !builder.is_absolute()
                    && builder
                        .components()
                        .all(|component| matches!(component, std::path::Component::Normal(_))),
                "app-owned rootfs builder `{}` must be relative to the app directory",
                builder.display()
            );
            let builder_path = app.case_dir.join(&builder);
            ensure!(
                builder_path.is_file(),
                "app-owned rootfs builder `{}` does not exist",
                builder_path.display()
            );
            let target_arch = config.target_arch.with_context(|| {
                format!(
                    "app-owned rootfs preparation in {} requires `target_arch`",
                    qemu_config_path.display()
                )
            })?;
            ensure!(
                !target_arch.trim().is_empty() && target_arch == target_arch.trim(),
                "app-owned rootfs target_arch must be non-empty and trimmed"
            );
            Ok(RootfsPreparation::AppOwned(AppOwnedRootfsPreparation {
                builder_path,
                target_arch,
            }))
        }
    }
}

fn qemu_app_config_rootfs_path(
    workspace_root: &Path,
    qemu_config_path: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    let qemu = read_qemu_app_config(qemu_config_path)?;
    Ok(qemu_app_managed_rootfs_paths(workspace_root, &qemu)?
        .into_iter()
        .next())
}

fn read_qemu_app_config(qemu_config_path: &Path) -> anyhow::Result<ostool::run::qemu::QemuConfig> {
    let content = fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))
}

fn qemu_app_managed_rootfs_paths(
    workspace_root: &Path,
    qemu: &ostool::run::qemu::QemuConfig,
) -> anyhow::Result<Vec<PathBuf>> {
    crate::rootfs::qemu::drive_file_paths(qemu)
        .into_iter()
        .filter_map(|path| {
            crate::image::storage::resolve_managed_rootfs_path(workspace_root, &path).transpose()
        })
        .collect()
}

pub(super) fn resolve_qemu_config(
    app: &StarryAppCase,
    arch: Option<&str>,
    explicit_qemu_config: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(path) = explicit_qemu_config {
        return Ok(Some(resolve_case_relative_path(&app.case_dir, path)));
    }

    let arch = arch.unwrap_or(DEFAULT_STARRY_ARCH);
    let path = app.case_dir.join(qemu_config_name(arch));
    if path.is_file() {
        return Ok(Some(path));
    }

    let variants = qemu_config_variants_for_arch(&app.case_dir, arch)?;
    if !variants.is_empty() {
        bail!(
            "Starry app `{}` does not provide `{}`; pass --qemu-config to select one of: {}",
            app.name,
            qemu_config_name(arch),
            format_paths(&variants)
        );
    }

    let configs = collect_prefixed_toml_files(&app.case_dir, "qemu-")?;
    if !configs.is_empty() {
        bail!(
            "Starry app `{}` does not provide `{}`; available QEMU configs: {}",
            app.name,
            qemu_config_name(arch),
            format_paths(&configs)
        );
    }
    Ok(None)
}

fn format_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn qemu_app_supports_arch(app: &StarryAppCase, arch: &str) -> bool {
    app.case_dir.join(qemu_config_name(arch)).is_file()
        || !qemu_config_variants_for_arch(&app.case_dir, arch)
            .unwrap_or_default()
            .is_empty()
}

fn qemu_config_name(arch: &str) -> String {
    format!("qemu-{arch}.toml")
}

fn qemu_config_variants_for_arch(case_dir: &Path, arch: &str) -> anyhow::Result<Vec<PathBuf>> {
    let prefix = format!("qemu-{arch}-");
    collect_prefixed_toml_files(case_dir, &prefix)
}

fn arch_from_qemu_config_path(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    let rest = stem.strip_prefix("qemu-")?;
    rest.split('-').next().filter(|arch| !arch.is_empty())
}

#[cfg(test)]
#[path = "tests/qemu.rs"]
mod tests;
// weave: run 'weave explain scripts/axbuild/src/starry/app/qemu.rs' for per-hunk detail, 'weave check' to verify your resolution
