use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};

pub(crate) fn workspace_root_path() -> anyhow::Result<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("failed to locate workspace root from axbuild crate")?;
    root.canonicalize()
        .context("failed to canonicalize workspace root")
}

pub(crate) fn workspace_member_dir(package: &str) -> anyhow::Result<PathBuf> {
    workspace_member_dir_in(&workspace_root_path()?, package)
}

pub(crate) fn workspace_member_dir_in(
    workspace_root: &Path,
    package: &str,
) -> anyhow::Result<PathBuf> {
    let manifest_path = workspace_member_manifest_path(workspace_root, package)?;
    manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("package manifest path has no parent directory"))
}

pub(crate) fn find_workspace_root() -> PathBuf {
    workspace_root_path().expect("failed to resolve workspace root")
}

pub(crate) fn workspace_manifest_path() -> anyhow::Result<PathBuf> {
    Ok(workspace_root_path()?.join("Cargo.toml"))
}

pub(crate) fn workspace_manifest_path_in(workspace_root: &Path) -> PathBuf {
    workspace_root.join("Cargo.toml")
}

pub(crate) fn workspace_metadata_root_manifest(
    workspace_manifest_path: &Path,
) -> anyhow::Result<cargo_metadata::Metadata> {
    cargo_metadata::MetadataCommand::new()
        .no_deps()
        .manifest_path(workspace_manifest_path)
        .exec()
        .with_context(|| {
            format!(
                "failed to get cargo metadata for workspace root {}",
                workspace_manifest_path.display()
            )
        })
}

fn workspace_member_manifest_path(workspace_root: &Path, package: &str) -> anyhow::Result<PathBuf> {
    let metadata = workspace_metadata(workspace_root)?;
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    metadata
        .packages
        .iter()
        .find(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package)
        .map(|pkg| pkg.manifest_path.clone().into_std_path_buf())
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))
}

fn workspace_metadata(workspace_root: &Path) -> anyhow::Result<cargo_metadata::Metadata> {
    workspace_metadata_root_manifest(&workspace_manifest_path_in(workspace_root))
}
