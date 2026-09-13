use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};

use anyhow::bail;
use cargo_metadata::{Metadata, Package};

use super::AXSTD_STD_PACKAGE;

const INCREMENTAL_CLIPPY_OS_ROOT_PACKAGES: &[&str] = &[
    AXSTD_STD_PACKAGE,
    crate::context::STARRY_KERNEL_PACKAGE,
    crate::context::STARRY_PACKAGE,
];

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

pub(super) fn resolve_requested_packages(
    args: &crate::ClippyArgs,
    workspace_root: &Path,
    metadata: &Metadata,
    all_packages: &[Package],
) -> anyhow::Result<Vec<Package>> {
    let package_lookup: HashMap<_, _> = all_packages
        .iter()
        .map(|pkg| (pkg.name.as_str(), pkg.clone()))
        .collect();
    let known_packages: HashSet<_> = all_packages.iter().map(|pkg| pkg.name.as_str()).collect();

    let selections: Vec<String> = if !args.packages.is_empty() {
        validate_requested_packages(&args.packages, &known_packages)?
    } else if args.all {
        all_packages
            .iter()
            .map(|pkg| pkg.name.to_string())
            .collect()
    } else if let Some(since) = args.since.as_deref() {
        match crate::support::git::select_incremental_packages(
            workspace_root,
            metadata,
            all_packages,
            since,
        )? {
            crate::support::git::IncrementalPackageSelection::Packages { changed, affected } => {
                let changed_count = changed.iter().collect::<BTreeSet<_>>().len();
                let selections = incremental_clippy_selections(changed, affected);
                println!(
                    "incremental clippy since `{since}` selected {} changed package(s) and {} \
                     affected OS root package(s)",
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
                    .map(|pkg| pkg.name.to_string())
                    .collect()
            }
        }
    } else {
        all_packages
            .iter()
            .map(|pkg| pkg.name.to_string())
            .collect()
    };

    selections
        .into_iter()
        .map(|package| {
            let package = package_lookup
                .get(package.as_str())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))?;
            Ok(package)
        })
        .collect()
}

/// Build the incremental clippy selection from a `--since` diff.
///
/// Changed crates and affected OS roots are linted with `--no-deps` and their
/// full feature/target/configuration matrix. Intermediate reverse dependencies
/// are intentionally excluded because the selected OS roots cover integration.
pub(super) fn incremental_clippy_selections(
    changed: Vec<String>,
    affected: Vec<String>,
) -> Vec<String> {
    let changed = changed.into_iter().collect::<BTreeSet<_>>();
    let affected = affected.into_iter().collect::<BTreeSet<_>>();

    changed
        .iter()
        .cloned()
        .chain(
            INCREMENTAL_CLIPPY_OS_ROOT_PACKAGES
                .iter()
                .filter(|package| affected.contains(**package) && !changed.contains(**package))
                .map(|package| (*package).to_string()),
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

pub(super) fn skip_unsupported_packages(packages: Vec<Package>) -> Vec<Package> {
    packages
        .into_iter()
        .filter(|package| {
            if let Some(reason) = clippy_skip_reason(package) {
                println!("skipping clippy for package `{}`: {reason}", package.name);
                false
            } else {
                true
            }
        })
        .collect()
}
