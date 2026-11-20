use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{bail, ensure};
use cargo_metadata::{Metadata, Package};

use super::check::ClippyCheck;

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

pub(super) fn validate_clippy_args(args: &crate::ClippyArgs) -> anyhow::Result<()> {
    ensure!(args.jobs > 0, "`--jobs` must be greater than zero");
    ensure!(
        !args.all || args.packages.is_empty(),
        "`--all` cannot be combined with `--package`"
    );
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
    all_packages: &[Package],
) -> anyhow::Result<Vec<Package>> {
    let package_lookup: HashMap<_, _> = all_packages
        .iter()
        .map(|pkg| (pkg.name.as_str(), pkg.clone()))
        .collect();
    let known_packages: HashSet<_> = all_packages.iter().map(|pkg| pkg.name.as_str()).collect();

    let selections: Vec<String> = if !args.packages.is_empty() {
        validate_requested_packages(&args.packages, &known_packages)?
    } else {
        all_packages
            .iter()
            .map(|package| package.name.to_string())
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
