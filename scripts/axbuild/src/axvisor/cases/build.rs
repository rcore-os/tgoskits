use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use ostool::ctx::OutputArtifacts;
use serde::Deserialize;

use crate::{
    arceos::build::{
        ArceosBuildInfo, LogLevel, load_or_create_build_info, resolve_build_info_path_in_dir,
    },
    axvisor::{
        cases::{RunArtifacts, manifest::LoadedCase, rootfs},
        context::AxvisorContext,
    },
    context::{AppContext, target_for_arch_checked},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CaseLayout {
    pub(crate) case_dir: PathBuf,
    pub(crate) vm_template: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreparedCaseAssets {
    pub(crate) case_id: String,
    pub(crate) asset_key: String,
    pub(crate) vm_id: usize,
    pub(crate) package: String,
    pub(crate) target: String,
    pub(crate) build_info_path: PathBuf,
    pub(crate) host_case_dir: PathBuf,
    pub(crate) staged_kernel_host_path: PathBuf,
    pub(crate) rendered_vm_host_path: PathBuf,
    pub(crate) guest_kernel_path: String,
    pub(crate) guest_vm_config_path: String,
    pub(crate) runtime_artifact_path: PathBuf,
}

pub(super) fn resolve_case_layouts(
    cases: &[LoadedCase],
    arch: &str,
) -> anyhow::Result<Vec<CaseLayout>> {
    cases
        .iter()
        .map(|case| resolve_case_layout(case, arch))
        .collect()
}

fn resolve_case_layout(case: &LoadedCase, arch: &str) -> anyhow::Result<CaseLayout> {
    let vm_template = case.case_dir.join("vm").join(format!("{arch}.toml.in"));
    ensure_file_exists(&vm_template, "VM template", case)?;

    Ok(CaseLayout {
        case_dir: case.case_dir.clone(),
        vm_template,
    })
}

pub(super) async fn build_and_stage_cases(
    app: &mut AppContext,
    _ctx: &AxvisorContext,
    cases: &[LoadedCase],
    layouts: &[CaseLayout],
    artifacts: &RunArtifacts,
    arch: &str,
) -> anyhow::Result<Vec<PreparedCaseAssets>> {
    if cases.len() != layouts.len() {
        bail!(
            "internal error: case/layout length mismatch (cases={}, layouts={})",
            cases.len(),
            layouts.len()
        );
    }

    let target = target_for_arch_checked(arch)?.to_string();
    let mut prepared = Vec::with_capacity(cases.len());

    for (case, layout) in cases.iter().zip(layouts) {
        let package = resolve_case_package_name(&case.case_dir)?;
        let build_info_path = resolve_build_info_path_in_dir(&case.case_dir, &target);
        let asset_key = sanitize_asset_key(&case.manifest.id);
        let vm_id = 1;
        let host_case_dir = artifacts.run_dir.join("cases").join(&asset_key);
        fs::create_dir_all(&host_case_dir)
            .with_context(|| format!("failed to create {}", host_case_dir.display()))?;

        let built = build_guest_case(app, &package, &target, &build_info_path).await?;
        let runtime_artifact_path = resolve_runtime_artifact_path(&built)?.to_path_buf();

        let staged_kernel_host_path = host_case_dir.join("kernel.bin");
        copy_file(&runtime_artifact_path, &staged_kernel_host_path)?;

        let guest_kernel_path = format!("/axcases/images/{asset_key}/kernel.bin");
        let rendered_vm_host_path = host_case_dir.join("vm.toml");
        render_vm_config(
            &layout.vm_template,
            &rendered_vm_host_path,
            vm_id,
            &guest_kernel_path,
        )?;

        let guest_vm_config_path = format!("/axcases/vm/{asset_key}.toml");
        rootfs::inject_host_file(
            &artifacts.target_rootfs,
            &guest_kernel_path,
            &staged_kernel_host_path,
        )?;
        rootfs::inject_host_file(
            &artifacts.target_rootfs,
            &guest_vm_config_path,
            &rendered_vm_host_path,
        )?;

        prepared.push(PreparedCaseAssets {
            case_id: case.manifest.id.clone(),
            asset_key,
            vm_id,
            package,
            target: target.clone(),
            build_info_path,
            host_case_dir,
            staged_kernel_host_path,
            rendered_vm_host_path,
            guest_kernel_path,
            guest_vm_config_path,
            runtime_artifact_path,
        });
    }

    Ok(prepared)
}

fn ensure_file_exists(path: &Path, kind: &str, case: &LoadedCase) -> anyhow::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        bail!(
            "{} missing for case `{}`: {}",
            kind,
            case.manifest.id,
            path.display()
        )
    }
}

async fn build_guest_case(
    app: &mut AppContext,
    package: &str,
    target: &str,
    build_info_path: &Path,
) -> anyhow::Result<OutputArtifacts> {
    let mut build_info: ArceosBuildInfo = load_or_create_build_info(build_info_path, || {
        ArceosBuildInfo::default_for_target(target)
    })?;
    build_info.log = LogLevel::Warn;
    let cargo = build_info.into_prepared_base_cargo_config(package, target, None)?;
    app.build_with_artifacts(cargo, build_info_path.to_path_buf())
        .await
}

pub(super) fn resolve_runtime_artifact_path(artifacts: &OutputArtifacts) -> anyhow::Result<&Path> {
    artifacts
        .bin
        .as_deref()
        .or(artifacts.elf.as_deref())
        .ok_or_else(|| anyhow!("build finished without runtime artifact path"))
}

fn resolve_case_package_name(case_dir: &Path) -> anyhow::Result<String> {
    let manifest_path = case_dir.join("Cargo.toml");
    let manifest = read_toml::<CargoManifest>(&manifest_path)?;
    let package = manifest.package.name.trim();
    if package.is_empty() {
        bail!(
            "case Cargo manifest {} has empty package.name",
            manifest_path.display()
        );
    }
    Ok(package.to_string())
}

fn sanitize_asset_key(case_id: &str) -> String {
    let mut result = String::with_capacity(case_id.len());
    for ch in case_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            result.push(ch);
        } else {
            result.push('_');
        }
    }
    if result.is_empty() {
        "case".to_string()
    } else {
        result
    }
}

fn copy_file(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(src, dst)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))?;
    Ok(())
}

fn render_vm_config(
    template_path: &Path,
    output_path: &Path,
    vm_id: usize,
    guest_kernel_path: &str,
) -> anyhow::Result<()> {
    let mut value = read_toml::<toml::Value>(template_path)?;
    value
        .get_mut("base")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow!("missing `[base]` section in {}", template_path.display()))?
        .insert(
            "id".to_string(),
            toml::Value::Integer(i64::try_from(vm_id).context("vm_id does not fit in i64")?),
        );
    value
        .get_mut("kernel")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow!("missing `[kernel]` section in {}", template_path.display()))?
        .insert(
            "kernel_path".to_string(),
            toml::Value::String(guest_kernel_path.to_string()),
        );
    write_toml(output_path, &value)
}

fn read_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

fn write_toml(path: &Path, value: &toml::Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, toml::to_string_pretty(value)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

#[derive(Debug, Deserialize)]
struct CargoManifest {
    package: CargoPackageSection,
}

#[derive(Debug, Deserialize)]
struct CargoPackageSection {
    name: String,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::axvisor::cases::manifest::CaseManifest;

    fn test_case(case_dir: PathBuf) -> LoadedCase {
        LoadedCase {
            case_dir,
            manifest: CaseManifest {
                id: "cpu.tlb".to_string(),
                tags: vec![],
                arch: vec!["aarch64".to_string()],
                timeout_secs: 5,
                description: None,
            },
        }
    }

    #[test]
    fn resolve_case_layout_requires_vm_template() {
        let dir = tempdir().unwrap();
        let case_dir = dir.path().join("case");
        fs::create_dir_all(case_dir.join("vm")).unwrap();
        fs::write(
            case_dir.join("vm/aarch64.toml.in"),
            "id=${vm_id}\nkernel_path=\"${kernel_path}\"\n",
        )
        .unwrap();

        let layout = resolve_case_layout(&test_case(case_dir.clone()), "aarch64").unwrap();
        assert_eq!(layout.vm_template, case_dir.join("vm/aarch64.toml.in"));
    }

    #[test]
    fn render_vm_config_replaces_placeholders() {
        let dir = tempdir().unwrap();
        let template = dir.path().join("vm.toml.in");
        let output = dir.path().join("vm.toml");
        fs::write(
            &template,
            r#"
[base]
id = 3

[kernel]
kernel_path = "/old/kernel.bin"
"#,
        )
        .unwrap();

        render_vm_config(&template, &output, 7, "/axcases/images/case/kernel.bin").unwrap();
        let rendered = fs::read_to_string(&output).unwrap();
        assert!(rendered.contains("id = 7"));
        assert!(rendered.contains("/axcases/images/case/kernel.bin"));
    }
}
