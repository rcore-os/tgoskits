use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use cargo_metadata::{Metadata, Package, PackageId};

use super::manifest::{ROOT_MANIFEST, RootManifestChange};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IncrementalPackageSelection {
    Packages {
        changed: Vec<String>,
        affected: Vec<String>,
    },
    Full {
        reason: String,
    },
}

#[cfg(test)]
pub(super) fn select_incremental_packages_for_paths<I>(
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

pub(super) fn select_incremental_packages_for_paths_with_root_manifest_change<I>(
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

    let changed_packages = filter_current_workspace_packages(workspace_packages, changed_packages);
    let affected = affected_workspace_packages(metadata, workspace_packages, &changed_packages);

    Ok(IncrementalPackageSelection::Packages {
        changed: changed_packages.into_iter().collect(),
        affected: affected.into_iter().collect(),
    })
}

fn filter_current_workspace_packages(
    workspace_packages: &[Package],
    packages: BTreeSet<String>,
) -> BTreeSet<String> {
    let current_packages = workspace_packages
        .iter()
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();
    packages
        .into_iter()
        .filter(|package| current_packages.contains(package.as_str()))
        .collect()
}

enum ChangedPackages {
    Packages(BTreeSet<String>),
    Full { path: PathBuf },
}

enum GlobalClippyInput {
    Hard,
    Soft,
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
) -> BTreeSet<String> {
    if changed_packages.is_empty() {
        return BTreeSet::new();
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

pub(crate) fn top_level_affected_workspace_packages(
    metadata: &Metadata,
    workspace_packages: &[Package],
    affected: &BTreeSet<String>,
) -> Vec<String> {
    if affected.is_empty() {
        return Vec::new();
    }

    let workspace_members = workspace_packages
        .iter()
        .map(|package| package.id.clone())
        .collect::<BTreeSet<_>>();
    let id_to_name = workspace_packages
        .iter()
        .map(|package| (package.id.clone(), package.name.to_string()))
        .collect::<BTreeMap<_, _>>();
    let name_to_id = id_to_name
        .iter()
        .map(|(id, name)| (name.clone(), id.clone()))
        .collect::<BTreeMap<_, _>>();
    let affected_ids = affected
        .iter()
        .filter_map(|name| name_to_id.get(name).cloned())
        .collect::<BTreeSet<_>>();

    let Some(resolve) = &metadata.resolve else {
        return affected.iter().cloned().collect();
    };

    // Forward dependency edges restricted to the affected set, plus the affected
    // crates that some other affected crate depends on.
    let mut affected_deps = BTreeMap::<PackageId, Vec<PackageId>>::new();
    let mut depended_on_by_affected = BTreeSet::new();
    for node in &resolve.nodes {
        if !workspace_members.contains(&node.id) || !affected_ids.contains(&node.id) {
            continue;
        }
        let deps = node
            .deps
            .iter()
            .map(|dep| dep.pkg.clone())
            .filter(|pkg| affected_ids.contains(pkg))
            .collect::<Vec<_>>();
        for pkg in &deps {
            depended_on_by_affected.insert(pkg.clone());
        }
        affected_deps.insert(node.id.clone(), deps);
    }

    // Maximal crates (nothing in `affected` depends on them) cover the whole
    // affected set via their with-deps run — as long as the graph is a DAG. A
    // dependency cycle (only reachable through dev-dependencies) makes every
    // member "depended on", so a cycle sitting at the top would be dropped from
    // the frontier and silently left unlinted. Guarantee coverage instead: walk
    // the forward closure of the roots and promote any still-uncovered crate to
    // a root until every affected crate is reachable.
    let mut roots = affected_ids
        .difference(&depended_on_by_affected)
        .cloned()
        .collect::<Vec<_>>();
    let mut covered = BTreeSet::new();
    for root in &roots {
        extend_coverage(&affected_deps, root, &mut covered);
    }
    for id in &affected_ids {
        if !covered.contains(id) {
            roots.push(id.clone());
            extend_coverage(&affected_deps, id, &mut covered);
        }
    }

    roots.sort();
    roots
        .into_iter()
        .filter_map(|id| id_to_name.get(&id).cloned())
        .collect()
}

/// Mark `start` and every affected crate reachable from it (via the restricted
/// `affected_deps` edges) as covered. Cycle-safe: the `covered` set doubles as
/// the visited set.
fn extend_coverage(
    affected_deps: &BTreeMap<PackageId, Vec<PackageId>>,
    start: &PackageId,
    covered: &mut BTreeSet<PackageId>,
) {
    let mut stack = vec![start.clone()];
    while let Some(id) = stack.pop() {
        if covered.insert(id.clone())
            && let Some(deps) = affected_deps.get(&id)
        {
            stack.extend(deps.iter().cloned());
        }
    }
}
