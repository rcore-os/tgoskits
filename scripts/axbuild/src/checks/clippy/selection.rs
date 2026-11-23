use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};

use anyhow::{bail, ensure};
use cargo_metadata::{Metadata, Package};

use super::{AXSTD_STD_PACKAGE, check::ClippyCheck};

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
    if args.jobs == 0 {
        bail!("`--jobs` must be greater than zero");
    }
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

/// Reject catalogs that lose a whole package before assigning individual checks.
pub(super) fn validate_package_coverage(
    packages: &[Package],
    checks: &[ClippyCheck],
) -> anyhow::Result<()> {
    let expected = packages
        .iter()
        .filter(|package| clippy_skip_reason(package).is_none())
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();
    let actual = checks
        .iter()
        .map(|check| check.package.as_str())
        .collect::<BTreeSet<_>>();
    let missing = expected.difference(&actual).copied().collect::<Vec<_>>();
    let unexpected = actual.difference(&expected).copied().collect::<Vec<_>>();
    ensure!(
        missing.is_empty() && unexpected.is_empty(),
        "Clippy package coverage mismatch: missing [{}], unexpected [{}]",
        missing.join(", "),
        unexpected.join(", ")
    );
    println!(
        "CLIPPY_PACKAGE_COVERAGE: selected={} supported={} excluded={} checks={}",
        packages.len(),
        expected.len(),
        packages.len() - expected.len(),
        checks.len()
    );
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::checks::clippy::expand::expand_clippy_checks;

    #[test]
    fn discovered_members_cannot_disappear_from_the_expanded_catalog() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("Cargo.toml");
        fs::write(
            &manifest,
            "[workspace]\nmembers=[\"members/*\"]\nresolver=\"3\"\n",
        )
        .unwrap();
        let add_member = |name: &str| {
            let path = root.path().join("members").join(name);
            fs::create_dir_all(path.join("src")).unwrap();
            fs::write(
                path.join("Cargo.toml"),
                format!(
                    r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
[features]
capability = []
"#
                ),
            )
            .unwrap();
            fs::write(path.join("src/lib.rs"), "pub fn operation() {}\n").unwrap();
        };
        let metadata = || {
            cargo_metadata::MetadataCommand::new()
                .manifest_path(&manifest)
                .no_deps()
                .exec()
                .unwrap()
        };
        add_member("existing-member");
        let before = metadata();
        let checks = expand_clippy_checks(&workspace_packages(&before), &before).unwrap();
        validate_package_coverage(&workspace_packages(&before), &checks).unwrap();

        // The unchanged wildcard must discover a new member without another list.
        add_member("new-member");
        let after = metadata();
        let packages = workspace_packages(&after);
        let error = validate_package_coverage(&packages, &checks)
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing [new-member]"), "{error}");
        let mut checks = expand_clippy_checks(&packages, &after).unwrap();
        validate_package_coverage(&packages, &checks).unwrap();
        assert!(checks.iter().any(|check| check.package == "new-member"));
        checks[0].package = "not-a-member".into();
        let error = validate_package_coverage(&packages, &checks)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unexpected [not-a-member]"), "{error}");
    }
}
