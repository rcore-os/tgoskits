use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use cargo_metadata::{Metadata, Package, PackageId};
use toml::Value;

const ROOT_MANIFEST: &str = "Cargo.toml";
const WORKSPACE_TABLE: &str = "workspace";
const WORKSPACE_DEPENDENCIES_TABLE: &str = "dependencies";

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
) -> anyhow::Result<IncrementalPackageSelection> {
    let changed_paths = match changed_paths_since(workspace_root, since) {
        Ok(paths) => paths,
        Err(err) => {
            return Ok(IncrementalPackageSelection::Full {
                reason: format!("failed to diff against `{since}`: {err:#}"),
            });
        }
    };
    let root_manifest_change = if changed_paths
        .iter()
        .any(|path| path == Path::new(ROOT_MANIFEST))
    {
        Some(root_manifest_change_since(workspace_root, since)?)
    } else {
        None
    };

    select_incremental_packages_for_paths_with_root_manifest_change(
        workspace_root,
        metadata,
        workspace_packages,
        changed_paths,
        root_manifest_change,
    )
}

pub(crate) fn changed_paths_since(
    workspace_root: &Path,
    since: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    ensure_git_work_tree(workspace_root)?;

    let range = format!("{since}..HEAD");
    let output = Command::new("git")
        .args(git_safe_directory_args(workspace_root))
        .arg("-C")
        .arg(workspace_root)
        .args(["diff", "--name-only", range.as_str(), "--"])
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

fn ensure_git_work_tree(workspace_root: &Path) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(git_safe_directory_args(workspace_root))
        .arg("-C")
        .arg(workspace_root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .with_context(|| {
            format!(
                "failed to check whether {} is a git work tree",
                workspace_root.display()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "{} is not a git work tree{}",
            workspace_root.display(),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim() != "true" {
        bail!("{} is not inside a git work tree", workspace_root.display());
    }

    Ok(())
}

fn git_safe_directory_args(workspace_root: &Path) -> [String; 2] {
    [
        "-c".to_string(),
        format!("safe.directory={}", workspace_root.display()),
    ]
}

#[cfg(test)]
fn select_incremental_packages_for_paths<I>(
    workspace_root: &Path,
    metadata: &Metadata,
    workspace_packages: &[Package],
    changed_paths: I,
) -> anyhow::Result<IncrementalPackageSelection>
where
    I: IntoIterator<Item = PathBuf>,
{
    select_incremental_packages_for_paths_with_root_manifest_change(
        workspace_root,
        metadata,
        workspace_packages,
        changed_paths,
        None,
    )
}

fn select_incremental_packages_for_paths_with_root_manifest_change<I>(
    workspace_root: &Path,
    metadata: &Metadata,
    workspace_packages: &[Package],
    changed_paths: I,
    root_manifest_change: Option<RootManifestChange>,
) -> anyhow::Result<IncrementalPackageSelection>
where
    I: IntoIterator<Item = PathBuf>,
{
    let package_index = PackagePathIndex::new(workspace_root, workspace_packages)?;
    let changed_packages =
        match package_index.changed_packages(changed_paths, root_manifest_change)? {
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
    Ok(IncrementalPackageSelection::Packages(affected))
}

enum ChangedPackages {
    Packages(BTreeSet<String>),
    Full { path: PathBuf },
}

enum GlobalClippyInput {
    Hard,
    Soft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RootManifestChange {
    Hard,
    LocalWorkspaceDependencies(BTreeSet<String>),
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
                let manifest_dir = manifest_dir.canonicalize().with_context(|| {
                    format!(
                        "failed to canonicalize manifest dir for package `{}`: {}",
                        package.name,
                        manifest_dir.display()
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

    fn changed_packages<I>(
        &self,
        changed_paths: I,
        root_manifest_change: Option<RootManifestChange>,
    ) -> anyhow::Result<ChangedPackages>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut packages = BTreeSet::new();
        let mut soft_global_inputs = Vec::new();
        for path in changed_paths {
            let path = normalize_git_path(path)?;
            if path.as_os_str().is_empty() {
                continue;
            }
            if path == Path::new(ROOT_MANIFEST) {
                match root_manifest_change
                    .clone()
                    .unwrap_or(RootManifestChange::Hard)
                {
                    RootManifestChange::Hard => return Ok(ChangedPackages::Full { path }),
                    RootManifestChange::LocalWorkspaceDependencies(dependencies) => {
                        packages.extend(dependencies);
                        soft_global_inputs.push(path);
                    }
                }
                continue;
            }
            let Some(package) = self.package_for_path(&path) else {
                match global_clippy_input(&path) {
                    Some(GlobalClippyInput::Hard) => return Ok(ChangedPackages::Full { path }),
                    Some(GlobalClippyInput::Soft) => soft_global_inputs.push(path),
                    None => {}
                }
                continue;
            };
            packages.insert(package.to_string());
        }
        if packages.is_empty()
            && let Some(path) = soft_global_inputs.into_iter().next()
        {
            return Ok(ChangedPackages::Full { path });
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

fn global_clippy_input(path: &Path) -> Option<GlobalClippyInput> {
    if path == Path::new("Cargo.lock") {
        // Soft: a dep-version-only update (e.g. `cargo update`) is unlikely to
        // affect clippy when real code also changed.  When Cargo.lock is the
        // *only* change, however, transitive-dep/proc-macro/build-script changes
        // can still break compilation, so fall back to Full in that case.
        Some(GlobalClippyInput::Soft)
    } else if path == Path::new("rust-toolchain")
        || path == Path::new("rust-toolchain.toml")
        || path == Path::new("clippy.toml")
        || path == Path::new(".clippy.toml")
        || path.starts_with(".cargo")
        || path.starts_with("os/arceos/configs")
    {
        Some(GlobalClippyInput::Hard)
    } else {
        None
    }
}

fn root_manifest_change_since(
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

fn classify_root_manifest_change(
    old_manifest: &str,
    new_manifest: &str,
) -> anyhow::Result<RootManifestChange> {
    let old: Value = toml::from_str(old_manifest).context("failed to parse old root Cargo.toml")?;
    let new: Value = toml::from_str(new_manifest).context("failed to parse new root Cargo.toml")?;

    if without_workspace_dependencies(old.clone()) != without_workspace_dependencies(new.clone()) {
        return Ok(RootManifestChange::Hard);
    }

    let old_dependencies = workspace_dependencies(&old);
    let new_dependencies = workspace_dependencies(&new);
    let mut changed = BTreeSet::new();
    for dependency in old_dependencies
        .keys()
        .chain(new_dependencies.keys())
        .collect::<BTreeSet<_>>()
    {
        let old_dependency = old_dependencies.get(dependency.as_str());
        let new_dependency = new_dependencies.get(dependency.as_str());
        if old_dependency == new_dependency {
            continue;
        }
        if !old_dependency.is_some_and(is_local_workspace_dependency)
            && !new_dependency.is_some_and(is_local_workspace_dependency)
        {
            return Ok(RootManifestChange::Hard);
        }
        changed.insert(dependency.to_string());
    }

    Ok(RootManifestChange::LocalWorkspaceDependencies(changed))
}

fn without_workspace_dependencies(mut manifest: Value) -> Value {
    if let Some(workspace) = manifest
        .get_mut(WORKSPACE_TABLE)
        .and_then(Value::as_table_mut)
    {
        workspace.remove(WORKSPACE_DEPENDENCIES_TABLE);
    }
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
    fn changed_crate_selects_reverse_dependencies() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [PathBuf::from("crates/alpha/src/lib.rs")],
        )
        .unwrap();

        assert_eq!(
            selected,
            IncrementalPackageSelection::Packages(vec![
                "alpha".into(),
                "beta".into(),
                "gamma".into()
            ])
        );
    }

    #[test]
    fn changed_middle_crate_selects_itself_and_dependents() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [PathBuf::from("crates/beta/src/lib.rs")],
        )
        .unwrap();

        assert_eq!(
            selected,
            IncrementalPackageSelection::Packages(vec!["beta".into(), "gamma".into()])
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
        )
        .unwrap();

        assert_eq!(selected, IncrementalPackageSelection::Packages(Vec::new()));
    }

    #[test]
    fn lockfile_only_change_falls_back_to_full() {
        // Cargo.lock is Soft: a dep-version-only update with no source changes
        // can still affect compilation via transitive deps, proc macros, or
        // build scripts, so a pure lockfile diff must trigger a full run.
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [PathBuf::from("Cargo.lock")],
        )
        .unwrap();

        assert!(matches!(
            selected,
            IncrementalPackageSelection::Full { reason } if reason.contains("Cargo.lock")
        ));
    }

    #[test]
    fn lockfile_change_keeps_incremental_selection_when_packages_changed() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [
                PathBuf::from("Cargo.lock"),
                PathBuf::from("crates/beta/Cargo.toml"),
            ],
        )
        .unwrap();

        assert_eq!(
            selected,
            IncrementalPackageSelection::Packages(vec!["beta".into(), "gamma".into()])
        );
    }

    #[test]
    fn root_cargo_toml_only_falls_back_to_full() {
        // Root Cargo.toml is Hard: a manifest-only change with no code changes
        // (e.g. a [workspace.dependencies] bump) must still fall back to Full.
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [PathBuf::from("Cargo.toml")],
        )
        .unwrap();

        assert!(matches!(
            selected,
            IncrementalPackageSelection::Full { reason } if reason.contains("Cargo.toml")
        ));
    }

    #[test]
    fn root_cargo_toml_with_package_change_still_falls_back_to_full() {
        // Root Cargo.toml is Hard: even when package source files are also in the
        // diff (e.g. a new crate was added *and* a workspace dependency was
        // bumped), the global manifest change requires a full run.  We cannot
        // distinguish "only added a member" from "bumped a workspace dep" without
        // parsing diff hunks, so Hard must always win.
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [
                PathBuf::from("Cargo.toml"),
                PathBuf::from("crates/alpha/src/lib.rs"),
            ],
        )
        .unwrap();

        assert!(matches!(
            selected,
            IncrementalPackageSelection::Full { reason } if reason.contains("Cargo.toml")
        ));
    }

    #[test]
    fn root_cargo_toml_workspace_dependency_change_keeps_incremental_package_selection() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths_with_root_manifest_change(
            root.path(),
            &metadata,
            &workspace_packages,
            [
                PathBuf::from("Cargo.toml"),
                PathBuf::from("crates/beta/Cargo.toml"),
            ],
            Some(RootManifestChange::LocalWorkspaceDependencies(
                BTreeSet::from(["beta".to_string()]),
            )),
        )
        .unwrap();

        assert_eq!(
            selected,
            IncrementalPackageSelection::Packages(vec!["beta".into(), "gamma".into()])
        );
    }

    #[test]
    fn root_manifest_classifier_accepts_local_workspace_dependency_removal() {
        let old_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            alpha = { version = "0.1.0", path = "crates/alpha" }
            beta = { version = "0.1.0", path = "crates/beta" }
        "#;
        let new_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            alpha = { version = "0.1.0", path = "crates/alpha" }
        "#;

        let change = classify_root_manifest_change(old_manifest, new_manifest).unwrap();

        assert_eq!(
            change,
            RootManifestChange::LocalWorkspaceDependencies(BTreeSet::from(["beta".to_string()]))
        );
    }

    #[test]
    fn root_manifest_classifier_keeps_external_dependency_changes_hard() {
        let old_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            anyhow = "1.0"
        "#;
        let new_manifest = r#"
            [workspace]
            members = ["crates/alpha"]

            [workspace.dependencies]
            anyhow = "2.0"
        "#;

        let change = classify_root_manifest_change(old_manifest, new_manifest).unwrap();

        assert_eq!(change, RootManifestChange::Hard);
    }

    #[test]
    fn global_config_file_falls_back_to_full_run() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [PathBuf::from(".cargo/config.toml")],
        )
        .unwrap();

        assert!(matches!(
            selected,
            IncrementalPackageSelection::Full { reason } if reason.contains(".cargo")
        ));
    }

    #[test]
    fn unrelated_outside_package_file_selects_no_packages() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [PathBuf::from("docs/guide.md")],
        )
        .unwrap();

        assert_eq!(selected, IncrementalPackageSelection::Packages(Vec::new()));
    }

    #[test]
    fn unrelated_outside_package_file_does_not_hide_package_changes() {
        let (root, metadata, workspace_packages) = test_workspace();
        let selected = select_incremental_packages_for_paths(
            root.path(),
            &metadata,
            &workspace_packages,
            [
                PathBuf::from(".github/workflows/review.yml"),
                PathBuf::from("crates/beta/src/lib.rs"),
            ],
        )
        .unwrap();

        assert_eq!(
            selected,
            IncrementalPackageSelection::Packages(vec!["beta".into(), "gamma".into()])
        );
    }
}
