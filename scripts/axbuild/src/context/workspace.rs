use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use toml::Value;

const WORKSPACE_TABLE: &str = "workspace";
const ROOT_MANIFEST: &str = "Cargo.toml";

pub(crate) fn workspace_root_path() -> anyhow::Result<PathBuf> {
    workspace_root_path_from(&env::current_dir()?, Path::new(env!("CARGO_MANIFEST_DIR")))
}

pub(crate) fn workspace_root_path_from(
    runtime_dir: &Path,
    compile_manifest_dir: &Path,
) -> anyhow::Result<PathBuf> {
    if let Some(root) = runtime_workspace_root(runtime_dir)? {
        return Ok(root);
    }

    let root = compile_manifest_dir
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

pub(crate) fn axbuild_tmp_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("tmp").join("axbuild")
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

pub(crate) fn workspace_metadata_root_manifest_with_deps(
    workspace_manifest_path: &Path,
) -> anyhow::Result<cargo_metadata::Metadata> {
    cargo_metadata::MetadataCommand::new()
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

fn runtime_workspace_root(start: &Path) -> anyhow::Result<Option<PathBuf>> {
    let mut dir = start
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", start.display()))?;

    loop {
        if manifest_has_workspace(&dir.join(ROOT_MANIFEST))? {
            return Ok(Some(dir));
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

fn manifest_has_workspace(path: &Path) -> anyhow::Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }

    let manifest =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let manifest: Value =
        toml::from_str(&manifest).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(manifest
        .get(WORKSPACE_TABLE)
        .and_then(Value::as_table)
        .is_some())
}
