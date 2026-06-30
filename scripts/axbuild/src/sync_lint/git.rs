use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};
use cargo_metadata::{Metadata, Package};
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SyncLintSelection {
    All { reason: Option<String> },
    Files(Vec<PathBuf>),
}

pub(super) fn workspace_packages(metadata: &Metadata) -> Vec<Package> {
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    let mut packages: Vec<_> = metadata
        .packages
        .iter()
        .filter(|pkg| workspace_members.contains(&pkg.id))
        .cloned()
        .collect();
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    packages
}

fn package_dir(package: &Package) -> anyhow::Result<PathBuf> {
    let package_dir = package
        .manifest_path
        .clone()
        .into_std_path_buf()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("invalid manifest path for package `{}`", package.name))?;
    Ok(package_dir)
}

pub(super) fn workspace_rust_source_files(packages: &[Package]) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = BTreeSet::new();
    for package in packages {
        files.extend(rust_source_files(&package_dir(package)?));
    }
    Ok(files.into_iter().collect())
}

pub(super) fn select_sync_lint_files(
    workspace_root: &Path,
    packages: &[Package],
    since: Option<&str>,
) -> anyhow::Result<SyncLintSelection> {
    let Some(since) = since else {
        return Ok(SyncLintSelection::All { reason: None });
    };

    let changed_paths = match crate::support::git::changed_paths_since(workspace_root, since) {
        Ok(paths) => paths,
        Err(err) => {
            return Ok(SyncLintSelection::All {
                reason: Some(format!("failed to diff against `{since}`: {err:#}")),
            });
        }
    };

    select_sync_lint_files_for_paths(workspace_root, packages, changed_paths)
}

pub(super) fn select_sync_lint_files_for_paths<I>(
    workspace_root: &Path,
    packages: &[Package],
    changed_paths: I,
) -> anyhow::Result<SyncLintSelection>
where
    I: IntoIterator<Item = PathBuf>,
{
    let package_dirs = workspace_package_dirs(workspace_root, packages)?;
    let mut files = BTreeSet::new();

    for path in changed_paths {
        let path = normalize_changed_path(&path)?;
        if path.as_os_str().is_empty() {
            continue;
        }
        if !path.extension().is_some_and(|ext| ext == "rs") {
            continue;
        }
        let Some(_package_dir) = package_dir_for_path(&package_dirs, &path) else {
            return Ok(SyncLintSelection::All {
                reason: Some(format!(
                    "changed Rust path `{}` is outside any workspace package",
                    path.display()
                )),
            });
        };
        let absolute = workspace_root.join(&path);
        if absolute.is_file() {
            files.insert(absolute);
        }
    }

    Ok(SyncLintSelection::Files(files.into_iter().collect()))
}

fn workspace_package_dirs(
    workspace_root: &Path,
    packages: &[Package],
) -> anyhow::Result<Vec<PathBuf>> {
    let workspace_root = workspace_root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let mut dirs = packages
        .iter()
        .map(|package| {
            let manifest = package.manifest_path.clone().into_std_path_buf();
            let dir = manifest
                .parent()
                .ok_or_else(|| anyhow!("invalid manifest path for package `{}`", package.name))?;
            dir.strip_prefix(&workspace_root)
                .map(Path::to_path_buf)
                .with_context(|| {
                    format!(
                        "workspace package `{}` manifest {} is outside workspace root {}",
                        package.name,
                        manifest.display(),
                        workspace_root.display()
                    )
                })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    dirs.sort_by(|left, right| {
        right
            .components()
            .count()
            .cmp(&left.components().count())
            .then_with(|| left.cmp(right))
    });
    Ok(dirs)
}

fn package_dir_for_path<'a>(package_dirs: &'a [PathBuf], path: &Path) -> Option<&'a Path> {
    package_dirs
        .iter()
        .find(|dir| path == dir.as_path() || path.starts_with(dir))
        .map(PathBuf::as_path)
}

fn normalize_changed_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        bail!(
            "git diff returned absolute path `{}`; expected workspace-relative path",
            path.display()
        );
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::CurDir => {}
            _ => bail!("invalid changed path `{}`", path.display()),
        }
    }
    Ok(normalized)
}

fn rust_source_files(package_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in WalkDir::new(package_dir)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != "target")
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    files
}
