use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};

use anyhow::bail;
use cargo_metadata::{Metadata, Package};

use super::check::ClippyDepsMode;

const UNSUPPORTED_CLIPPY_PACKAGES: &[(&str, &str)] = &[
    (
        "axvisor",
        "requires an Axvisor target/build configuration; use the axvisor xtask flow",
    ),
    (
        "mingo",
        "requires the chainloader Makefile target, BSP features, and custom RUSTFLAGS",
    ),
];

pub(super) fn clippy_metadata_needs_deps(args: &crate::ClippyArgs) -> bool {
    args.since.is_some() && args.packages.is_empty() && !args.all
}

pub(super) fn validate_clippy_args(args: &crate::ClippyArgs) -> anyhow::Result<()> {
    if args.since.is_some() && !args.packages.is_empty() {
        bail!("`--since` cannot be combined with `--package`; choose one package selection mode");
    }
    if args.since.is_some() && args.all {
        bail!("`--since` cannot be combined with `--all`; choose one package selection mode");
    }
    Ok(())
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

#[derive(Debug, Clone)]
pub(super) struct SelectedClippyPackage {
    pub(super) package: Package,
    pub(super) deps_mode: ClippyDepsMode,
}

pub(super) fn resolve_requested_packages(
    args: &crate::ClippyArgs,
    workspace_root: &Path,
    metadata: &Metadata,
    all_packages: &[Package],
) -> anyhow::Result<Vec<SelectedClippyPackage>> {
    let package_lookup: HashMap<_, _> = all_packages
        .iter()
        .map(|pkg| (pkg.name.as_str(), pkg.clone()))
        .collect();
    let known_packages: HashSet<_> = all_packages.iter().map(|pkg| pkg.name.as_str()).collect();

    let selections: Vec<(String, ClippyDepsMode)> = if !args.packages.is_empty() {
        validate_requested_packages(&args.packages, &known_packages)?
            .into_iter()
            .map(|package| (package, ClippyDepsMode::NoDeps))
            .collect()
    } else if args.all {
        all_packages
            .iter()
            .map(|pkg| (pkg.name.to_string(), ClippyDepsMode::NoDeps))
            .collect()
    } else if let Some(since) = args.since.as_deref() {
        match crate::support::git::select_incremental_packages(
            workspace_root,
            metadata,
            all_packages,
            since,
        )? {
            crate::support::git::IncrementalPackageSelection::Packages { changed, affected } => {
                let selections =
                    incremental_clippy_selections(changed, affected, metadata, all_packages);
                let changed_count = selections
                    .iter()
                    .filter(|(_, mode)| matches!(mode, ClippyDepsMode::NoDeps))
                    .count();
                println!(
                    "incremental clippy since `{since}` selected {} changed package(s) and {} \
                     dependent top-level package(s)",
                    changed_count,
                    selections.len() - changed_count
                );
                selections
            }
            crate::support::git::IncrementalPackageSelection::Full { reason } => {
                println!(
                    "incremental clippy since `{since}` fell back to full workspace: {reason}"
                );
                all_packages
                    .iter()
                    .map(|pkg| (pkg.name.to_string(), ClippyDepsMode::NoDeps))
                    .collect()
            }
        }
    } else {
        all_packages
            .iter()
            .map(|pkg| (pkg.name.to_string(), ClippyDepsMode::NoDeps))
            .collect()
    };

    selections
        .into_iter()
        .map(|(package, deps_mode)| {
            let package = package_lookup
                .get(package.as_str())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))?;
            Ok(SelectedClippyPackage { package, deps_mode })
        })
        .collect()
}

/// Build the incremental clippy selection from a `--since` diff.
///
/// Changed crates are linted with `--no-deps` (their own code, full feature
/// matrix). The runnable top-level frontier of the affected set is linted
/// *with* deps: `cargo clippy -p <crate>` lints every workspace member in that
/// crate's dependency subtree (cargo does not cap-lints path/workspace deps), so
/// one with-deps run covers the whole affected subtree below it.
///
/// The frontier is computed over `affected \ skipped`: an unsupported crate
/// (e.g. `axvisor`) cannot run through this flow and can be the *only* route
/// into part of the affected subtree, so removing it first re-promotes the
/// crates it would otherwise orphan to their own runnable roots. Skipped crates
/// are kept in `changed` on purpose: `skip_unsupported_packages` drops them
/// later with a consistent skip message.
pub(super) fn incremental_clippy_selections(
    changed: Vec<String>,
    affected: Vec<String>,
    metadata: &Metadata,
    all_packages: &[Package],
) -> Vec<(String, ClippyDepsMode)> {
    let skipped = all_packages
        .iter()
        .filter(|package| clippy_skip_reason(package).is_some())
        .map(|package| package.name.as_str())
        .collect::<HashSet<_>>();
    let changed_set = changed.iter().cloned().collect::<BTreeSet<_>>();

    let runnable_affected = affected
        .into_iter()
        .filter(|package| !skipped.contains(package.as_str()))
        .collect::<BTreeSet<_>>();
    let integration = crate::support::git::top_level_affected_workspace_packages(
        metadata,
        all_packages,
        &runnable_affected,
    );

    changed
        .into_iter()
        .map(|package| (package, ClippyDepsMode::NoDeps))
        .chain(
            integration
                .into_iter()
                .filter(|package| !changed_set.contains(package))
                .map(|package| (package, ClippyDepsMode::WithDeps)),
        )
        .collect()
}

pub(super) fn validate_requested_packages(
    requested: &[String],
    known_packages: &HashSet<&str>,
) -> anyhow::Result<Vec<String>> {
    let mut unique = HashSet::new();
    let mut packages = Vec::new();

    for package in requested {
        if !known_packages.contains(package.as_str()) {
            bail!("unknown workspace package `{package}` requested via --package");
        }
        if !unique.insert(package.as_str()) {
            bail!("duplicate workspace package `{package}` requested via --package");
        }
        packages.push(package.clone());
    }

    Ok(packages)
}

fn clippy_skip_reason(package: &Package) -> Option<&str> {
    UNSUPPORTED_CLIPPY_PACKAGES
        .iter()
        .find_map(|(name, reason)| (package.name == *name).then_some(*reason))
}

pub(super) fn skip_unsupported_packages(
    packages: Vec<SelectedClippyPackage>,
) -> Vec<SelectedClippyPackage> {
    packages
        .into_iter()
        .filter(|package| {
            if let Some(reason) = clippy_skip_reason(&package.package) {
                println!(
                    "skipping clippy for package `{}`: {reason}",
                    package.package.name
                );
                false
            } else {
                true
            }
        })
        .collect()
}
