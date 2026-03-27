use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};

pub(crate) fn workspace_root_path() -> anyhow::Result<PathBuf> {
    let cargo = workspace_metadata()?;

    cargo
        .workspace_root
        .canonicalize()
        .context("failed to canonicalize workspace root")
}

pub(crate) fn workspace_member_dir(package: &str) -> anyhow::Result<PathBuf> {
    let manifest_path = workspace_member_manifest_path(package)?;
    manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("package manifest path has no parent directory"))
}

pub(crate) fn find_workspace_root() -> PathBuf {
    workspace_root_path().expect("failed to resolve workspace root")
}

fn workspace_member_manifest_path(package: &str) -> anyhow::Result<PathBuf> {
    let metadata = workspace_metadata()?;
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    metadata
        .packages
        .iter()
        .find(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package)
        .map(|pkg| pkg.manifest_path.clone().into_std_path_buf())
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))
}

fn workspace_metadata() -> anyhow::Result<cargo_metadata::Metadata> {
    cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .context("failed to get cargo metadata")
}
