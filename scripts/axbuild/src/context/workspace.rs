use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug)]
pub(crate) struct WorkspaceContext {
    root: PathBuf,
    metadata: cargo_metadata::Metadata,
    target_dir: PathBuf,
}

impl WorkspaceContext {
    pub(crate) fn discover(target_dir: Option<&Path>) -> anyhow::Result<Self> {
        Self::from_root(&workspace_root_path()?, target_dir)
    }

    pub(crate) fn from_root(
        workspace_root: &Path,
        target_dir: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let root = workspace_root
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
        let metadata = workspace_metadata_root_manifest(&workspace_manifest_path_in(&root))?;
        let target_dir = target_dir.map_or_else(
            || metadata.target_directory.clone().into_std_path_buf(),
            |path| {
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    root.join(path)
                }
            },
        );

        Ok(Self {
            root,
            metadata,
            target_dir,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn metadata(&self) -> &cargo_metadata::Metadata {
        &self.metadata
    }

    pub(crate) fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    pub(crate) fn axbuild_artifact_dir(&self) -> PathBuf {
        self.target_dir.join("axbuild")
    }
}

pub(crate) fn resolve_axbuild_artifact_path(
    workspace_root: &Path,
    target_dir: &Path,
    path: &Path,
) -> PathBuf {
    let logical_root = workspace_root.join("target").join("axbuild");
    path.strip_prefix(&logical_root).map_or_else(
        |_| path.to_path_buf(),
        |relative| target_dir.join("axbuild").join(relative),
    )
}

pub(crate) fn workspace_root_path() -> anyhow::Result<PathBuf> {
    workspace_root_path_from(&env::current_dir()?)
}

pub(crate) fn workspace_root_path_from(runtime_dir: &Path) -> anyhow::Result<PathBuf> {
    #[derive(Deserialize)]
    struct LocatedProject {
        root: PathBuf,
    }
    // Delegate workspace membership, exclusions and package.workspace to Cargo.
    let output = Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format=json"])
        .current_dir(runtime_dir)
        .output()
        .context("failed to locate the invocation's Cargo workspace")?;
    anyhow::ensure!(
        output.status.success(),
        "failed to locate Cargo project: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let located: LocatedProject =
        serde_json::from_slice(&output.stdout).context("invalid cargo locate-project output")?;
    located
        .root
        .parent()
        .context("located manifest has no parent")?
        .canonicalize()
        .context("failed to canonicalize Cargo workspace root")
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
    let workspace_root = workspace_manifest_path
        .parent()
        .context("workspace manifest path has no parent")?;
    cargo_metadata::MetadataCommand::new()
        .current_dir(workspace_root)
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
    let workspace_root = workspace_manifest_path
        .parent()
        .context("workspace manifest path has no parent")?;
    cargo_metadata::MetadataCommand::new()
        .current_dir(workspace_root)
        .manifest_path(workspace_manifest_path)
        .exec()
        .with_context(|| {
            format!(
                "failed to get cargo metadata for workspace root {}",
                workspace_manifest_path.display()
            )
        })
}
