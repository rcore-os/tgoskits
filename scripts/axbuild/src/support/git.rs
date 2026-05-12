use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use cargo_metadata::{Metadata, Package, PackageId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IncrementalPackageSelection {
    Packages(Vec<String>),
    Full { reason: String },
}

pub(crate) fn select_incremental_packages(
    workspace_root: &Path,
    metadata: &Metadata,
    workspace_packages: &[Package],
    since: &str,
    whitelist: &[String],
) -> anyhow::Result<IncrementalPackageSelection> {
    let changed_paths = match changed_paths_since(workspace_root, since) {
        Ok(paths) => paths,
        Err(err) => {
            return Ok(IncrementalPackageSelection::Full {
                reason: format!("failed to diff against `{since}`: {err:#}"),
            });
        }
    };
    select_incremental_packages_for_paths(
        workspace_root,
        metadata,
        workspace_packages,
        changed_paths,
        whitelist,
    )
}

pub(crate) fn changed_paths_since(
    workspace_root: &Path,
    since: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    let range = format!("{since}..HEAD");
    let output = Command::new("git")
        .args(["diff", "--name-only", range.as_str(), "--"])
        .current_dir(workspace_root)
        .output()
        .with_context(|| format!("failed to run git diff for `{range}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git diff exited with status {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect())
}

pub(crate) fn select_incremental_packages_for_paths<I>(
    workspace_root: &Path,
    metadata: &Metadata,
    workspace_packages: &[Package],
    changed_paths: I,
    whitelist: &[String],
) -> anyhow::Result<IncrementalPackageSelection>
where
    I: IntoIterator<Item = PathBuf>,
{
    let package_index = PackagePathIndex::new(workspace_root, workspace_packages)?;
    let changed_packages = match package_index.changed_packages(changed_paths)? {
        ChangedPackages::Packages(packages) => packages,
        ChangedPackages::Full { path } => {
            return Ok(IncrementalPackageSelection::Full {
                reason: format!(
                    "changed path `{}` is outside any workspace package",
                    path.display()
                ),
            });
        }
    };

    let affected = affected_workspace_packages(metadata, workspace_packages, &changed_packages);
    let whitelist: BTreeSet<_> = whitelist.iter().cloned().collect();
    let selected = affected
        .into_iter()
        .filter(|package| whitelist.contains(package))
        .collect::<Vec<_>>();

    Ok(IncrementalPackageSelection::Packages(selected))
}

enum ChangedPackages {
    Packages(BTreeSet<String>),
    Full { path: PathBuf },
}

struct PackagePathIndex {
    packages: Vec<PackagePathEntry>,
}

struct PackagePathEntry {
    name: String,
    rel_dir: PathBuf,
}

impl PackagePathIndex {
    fn new(workspace_root: &Path, workspace_packages: &[Package]) -> anyhow::Result<Self> {
        let workspace_root = workspace_root
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
        let mut packages = workspace_packages
            .iter()
            .map(|package| {
                let manifest = package.manifest_path.clone().into_std_path_buf();
                let manifest_dir = manifest.parent().ok_or_else(|| {
                    anyhow::anyhow!(
                        "manifest path has no parent for package `{}`: {}",
                        package.name,
                        manifest.display()
                    )
                })?;
                let rel_dir = manifest_dir
                    .strip_prefix(&workspace_root)
                    .with_context(|| {
                        format!(
                            "workspace package `{}` manifest {} is outside workspace root {}",
                            package.name,
                            manifest.display(),
                            workspace_root.display()
                        )
                    })?;
                Ok(PackagePathEntry {
                    name: package.name.to_string(),
                    rel_dir: rel_dir.to_path_buf(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        packages.sort_by(|left, right| {
            right
                .rel_dir
                .components()
                .count()
                .cmp(&left.rel_dir.components().count())
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(Self { packages })
    }

    fn changed_packages<I>(&self, changed_paths: I) -> anyhow::Result<ChangedPackages>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut packages = BTreeSet::new();
        for path in changed_paths {
            let path = normalize_git_path(path)?;
            if path.as_os_str().is_empty() {
                continue;
            }
            let Some(package) = self.package_for_path(&path) else {
                return Ok(ChangedPackages::Full { path });
            };
            packages.insert(package.to_string());
        }
        Ok(ChangedPackages::Packages(packages))
    }

    fn package_for_path(&self, path: &Path) -> Option<&str> {
        self.packages
            .iter()
            .find(|package| path == package.rel_dir || path.starts_with(&package.rel_dir))
            .map(|package| package.name.as_str())
    }
}

fn normalize_git_path(path: PathBuf) -> anyhow::Result<PathBuf> {
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

fn affected_workspace_packages(
    metadata: &Metadata,
    workspace_packages: &[Package],
    changed_packages: &BTreeSet<String>,
) -> Vec<String> {
    if changed_packages.is_empty() {
        return Vec::new();
    }

    let workspace_members: BTreeSet<_> = workspace_packages
        .iter()
        .map(|package| package.id.clone())
        .collect();
    let id_to_name = workspace_packages
        .iter()
        .map(|package| (package.id.clone(), package.name.to_string()))
        .collect::<BTreeMap<_, _>>();
    let name_to_id = id_to_name
        .iter()
        .map(|(id, name)| (name.clone(), id.clone()))
        .collect::<BTreeMap<_, _>>();

    let Some(resolve) = &metadata.resolve else {
        return changed_packages.iter().cloned().collect();
    };

    let mut reverse_deps = BTreeMap::<PackageId, BTreeSet<PackageId>>::new();
    for node in &resolve.nodes {
        if !workspace_members.contains(&node.id) {
            continue;
        }
        for dep in &node.deps {
            if workspace_members.contains(&dep.pkg) {
                reverse_deps
                    .entry(dep.pkg.clone())
                    .or_default()
                    .insert(node.id.clone());
            }
        }
    }

    let mut affected = BTreeSet::new();
    let mut stack = changed_packages
        .iter()
        .filter_map(|name| name_to_id.get(name).cloned())
        .collect::<Vec<_>>();
    while let Some(package_id) = stack.pop() {
        if !affected.insert(package_id.clone()) {
            continue;
        }
        if let Some(dependents) = reverse_deps.get(&package_id) {
            stack.extend(dependents.iter().cloned());
        }
    }

    affected
        .into_iter()
        .filter_map(|id| id_to_name.get(&id).cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::Path};

    use super::*;

    fn package(root: &Path, name: &str, deps: &[&str]) -> serde_json::Value {
        let root = root.display().to_string();
        serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "id": format!("{name} 0.1.0 (path+file://{root}/crates/{name})"),
            "license": null,
            "license_file": null,
            "description": null,
            "source": null,
            "dependencies": deps
                .iter()
                .map(|dep| {
                    serde_json::json!({
                        "name": dep,
                        "source": null,
                        "req": "*",
                        "kind": null,
                        "rename": null,
                        "optional": false,
                        "uses_default_features": true,
                        "features": [],
                        "target": null,
                        "registry": null,
                        "path": format!("{root}/crates/{dep}")
                    })
                })
                .collect::<Vec<_>>(),
            "targets": [{
                "kind": ["lib"],
                "crate_types": ["lib"],
                "name": name,
                "src_path": format!("{root}/crates/{name}/src/lib.rs"),
                "edition": "2021",
                "doc": true,
                "doctest": true,
                "test": true
            }],
            "features": HashMap::<String, Vec<String>>::new(),
            "manifest_path": format!("{root}/crates/{name}/Cargo.toml"),
            "metadata": null,
            "publish": null,
            "authors": [],
            "categories": [],
            "keywords": [],
            "readme": null,
            "repository": null,
            "homepage": null,
            "documentation": null,
            "edition": "2021",
            "links": null,
            "default_run": null,
            "rust_version": null
        })
    }

    fn test_workspace() -> (tempfile::TempDir, Metadata, Vec<Package>) {
        let root = tempfile::tempdir().unwrap();
        for package in ["alpha", "beta", "gamma"] {
            std::fs::create_dir_all(root.path().join("crates").join(package).join("src")).unwrap();
        }
        let root_url = root.path().display().to_string();
        let alpha = format!("alpha 0.1.0 (path+file://{root_url}/crates/alpha)");
        let beta = format!("beta 0.1.0 (path+file://{root_url}/crates/beta)");
        let gamma = format!("gamma 0.1.0 (path+file://{root_url}/crates/gamma)");
        let value = serde_json::json!({
            "packages": [
                package(root.path(), "alpha", &[]),
                package(root.path(), "beta", &["alpha"]),
                package(root.path(), "gamma", &["beta"]),
            ],
            "workspace_members": [alpha, beta, gamma],
            "workspace_default_members": [alpha, beta, gamma],
            "resolve": {
                "nodes": [
                    {
                        "id": alpha,
                        "dependencies": [],
                        "deps": [],
                        "features": []
                    },
                    {
                        "id": beta,
                        "dependencies": [alpha],
                        "deps": [{
                            "name": "alpha",
                            "pkg": alpha,
                            "dep_kinds": [{"kind": null, "target": null}]
                        }],
                        "features": []
                    },
                    {
                        "id": gamma,
                        "dependencies": [beta],
                        "deps": [{
                            "name": "beta",
                            "pkg": beta,
                            "dep_kinds": [{"kind": null, "target": null}]
                        }],
                        "features": []
                    }
                ],
                "root": null
            },
            "target_directory": root.path().join("target"),
            "version": 1,
            "workspace_root": root.path(),
            "metadata": null,
        });
        let metadata: Metadata = serde_json::from_value(value).unwrap();
        let workspace_packages = metadata.packages.clone();
        (root, metadata, workspace_packages)
    }

    #[test]
    fn changed_crate_selects_reverse_dependencies_intersected_with_whitelist() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [PathBuf::from("crates/alpha/src/lib.rs")],
            &["alpha".into(), "gamma".into()],
        )
        .unwrap();

        assert_eq!(
            selected,
            IncrementalPackageSelection::Packages(vec!["alpha".into(), "gamma".into()])
        );
    }

    #[test]
    fn changed_unlisted_crate_still_checks_whitelisted_dependents() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [PathBuf::from("crates/alpha/src/lib.rs")],
            &["beta".into()],
        )
        .unwrap();

        assert_eq!(
            selected,
            IncrementalPackageSelection::Packages(vec!["beta".into()])
        );
    }

    #[test]
    fn no_changes_selects_no_packages() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            Vec::<PathBuf>::new(),
            &["alpha".into(), "beta".into(), "gamma".into()],
        )
        .unwrap();

        assert_eq!(selected, IncrementalPackageSelection::Packages(Vec::new()));
    }

    #[test]
    fn non_crate_file_falls_back_to_full_run() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [PathBuf::from("Cargo.lock")],
            &["alpha".into()],
        )
        .unwrap();

        assert!(matches!(
            selected,
            IncrementalPackageSelection::Full { reason } if reason.contains("Cargo.lock")
        ));
    }

    #[test]
    fn global_config_file_falls_back_to_full_run() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [PathBuf::from(".cargo/config.toml")],
            &["alpha".into()],
        )
        .unwrap();

        assert!(matches!(
            selected,
            IncrementalPackageSelection::Full { reason } if reason.contains(".cargo/config.toml")
        ));
    }
}
