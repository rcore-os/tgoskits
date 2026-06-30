use std::{collections::BTreeSet, fs, path::Path, process::Command};

use anyhow::{Context, bail};
use toml::Value;

use super::refs::git_safe_directory_args;

pub(super) const ROOT_MANIFEST: &str = "Cargo.toml";
const WORKSPACE_TABLE: &str = "workspace";
const WORKSPACE_DEPENDENCIES_TABLE: &str = "dependencies";
const WORKSPACE_PACKAGE_TABLE: &str = "package";
const WORKSPACE_METADATA_TABLE: &str = "metadata";
const PROFILE_TABLE: &str = "profile";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RootManifestChange {
    Hard,
    LocalWorkspaceDependencies(BTreeSet<String>),
}

pub(super) fn root_manifest_change_since(
    workspace_root: &Path,
    since: &str,
) -> anyhow::Result<RootManifestChange> {
    let old_manifest = git_show_file(workspace_root, since, ROOT_MANIFEST).with_context(|| {
        format!("failed to read `{ROOT_MANIFEST}` from `{since}` for incremental clippy")
    })?;
    let new_manifest =
        fs::read_to_string(workspace_root.join(ROOT_MANIFEST)).with_context(|| {
            format!(
                "failed to read current `{}`",
                workspace_root.join(ROOT_MANIFEST).display()
            )
        })?;

    classify_root_manifest_change(&old_manifest, &new_manifest).with_context(|| {
        format!("failed to classify `{ROOT_MANIFEST}` changes for incremental clippy")
    })
}

fn git_show_file(workspace_root: &Path, rev: &str, path: &str) -> anyhow::Result<String> {
    let spec = format!("{rev}:{path}");
    let output = Command::new("git")
        .args(git_safe_directory_args(workspace_root))
        .arg("-C")
        .arg(workspace_root)
        .args(["show", spec.as_str()])
        .output()
        .with_context(|| format!("failed to run git show `{spec}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git show `{spec}` exited with status {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    String::from_utf8(output.stdout).context("git show output was not valid UTF-8")
}

pub(super) fn classify_root_manifest_change(
    old_manifest: &str,
    new_manifest: &str,
) -> anyhow::Result<RootManifestChange> {
    let old: Value = toml::from_str(old_manifest).context("failed to parse old root Cargo.toml")?;
    let new: Value = toml::from_str(new_manifest).context("failed to parse new root Cargo.toml")?;

    if dependency_resolution_surface(old.clone()) != dependency_resolution_surface(new.clone()) {
        return Ok(RootManifestChange::Hard);
    }

    let old_dependencies = workspace_dependencies(&old);
    let new_dependencies = workspace_dependencies(&new);
    let mut changed = BTreeSet::new();
    for dependency_key in old_dependencies
        .keys()
        .chain(new_dependencies.keys())
        .collect::<BTreeSet<_>>()
    {
        let old_dependency = old_dependencies.get(dependency_key.as_str());
        let new_dependency = new_dependencies.get(dependency_key.as_str());
        if old_dependency == new_dependency {
            continue;
        }
        let old_package = old_dependency.and_then(|dependency| {
            local_workspace_dependency_package_name(dependency_key, dependency)
        });
        let new_package = new_dependency.and_then(|dependency| {
            local_workspace_dependency_package_name(dependency_key, dependency)
        });
        if old_package.is_none() && new_package.is_none() {
            return Ok(RootManifestChange::Hard);
        }
        changed.extend(old_package);
        changed.extend(new_package);
    }

    Ok(RootManifestChange::LocalWorkspaceDependencies(changed))
}

fn dependency_resolution_surface(mut manifest: Value) -> Value {
    if let Some(workspace) = manifest
        .get_mut(WORKSPACE_TABLE)
        .and_then(Value::as_table_mut)
    {
        workspace.remove(WORKSPACE_DEPENDENCIES_TABLE);
        workspace.remove(WORKSPACE_PACKAGE_TABLE);
        workspace.remove(WORKSPACE_METADATA_TABLE);
    }
    manifest
        .as_table_mut()
        .map(|table| table.remove(PROFILE_TABLE));
    manifest
}

fn workspace_dependencies(manifest: &Value) -> toml::Table {
    manifest
        .get(WORKSPACE_TABLE)
        .and_then(|workspace| workspace.get(WORKSPACE_DEPENDENCIES_TABLE))
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default()
}

fn is_local_workspace_dependency(dependency: &Value) -> bool {
    dependency
        .as_table()
        .and_then(|table| table.get("path"))
        .is_some_and(Value::is_str)
}

fn local_workspace_dependency_package_name(
    dependency_key: &str,
    dependency: &Value,
) -> Option<String> {
    if !is_local_workspace_dependency(dependency) {
        return None;
    }

    Some(
        dependency
            .as_table()
            .and_then(|table| table.get("package"))
            .and_then(Value::as_str)
            .unwrap_or(dependency_key)
            .to_string(),
    )
}
