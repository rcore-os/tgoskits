use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};

use super::{
    StarryAppQemuCase,
    build_config::{collect_prefixed_toml_files, discover_optional_build_config},
    discovery::resolve_case_relative_path,
    rootfs::prepare_qemu_app_rootfs,
    types::{StarryAppCase, StarryAppKind},
};
use crate::{
    context::starry_target_for_arch_checked,
    test::{
        case::TestQemuCase,
        qemu::{self as qemu_test},
    },
};

#[derive(Debug)]
struct LoadedQemuAppCaseFields {
    test_case: TestQemuCase,
    rootfs_path: Option<PathBuf>,
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
        .unwrap_or_else(|| "x86_64".to_string());
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
    )
    .await?;

    Ok(StarryAppQemuCase {
        name: app.name.clone(),
        arch,
        target,
        build_config_path,
        qemu_config_path,
        rootfs_path,
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
    let test_case = qemu_test::load_test_qemu_case_fields(
        app.name.clone(),
        app.name.clone(),
        app.case_dir.clone(),
        qemu_config_path.to_path_buf(),
        "Starry app",
        true,
    )?;
    let rootfs_path = qemu_app_config_rootfs_path(workspace_root, qemu_config_path)?;

    Ok(LoadedQemuAppCaseFields {
        test_case,
        rootfs_path,
    })
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

    let arch = arch.unwrap_or("x86_64");
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
