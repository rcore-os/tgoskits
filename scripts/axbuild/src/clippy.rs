use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use ax_config_gen::read_config_string;
use cargo_metadata::{Metadata, Package};
use serde_json::Value;

use crate::support::process::run_cargo_status_with_env;

const DEFAULT_FEATURE: &str = "default";
const AX_CONFIG_PATH_ENV: &str = "AX_CONFIG_PATH";
const AXCONFIG_FILE: &str = "axconfig.toml";
const ARCEOS_RUST_PACKAGE: &str = "arceos-rust";
const ARCEOS_RUST_CLIPPY_TARGET: &str = "x86_64-unknown-none";
const DOCS_RS_METADATA: &str = "docs.rs";
const DOCS_METADATA: &str = "docs";
const RS_METADATA: &str = "rs";
const TARGETS_METADATA: &str = "targets";
const AXBUILD_METADATA: &str = "axbuild";
const CLIPPY_FEATURE_AXCONFIG_OVERRIDES_METADATA: &str = "clippy-feature-axconfig-overrides";

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

const CLIPPY_TARGET_ALIASES: &[(&str, &str)] = &[
    (
        "aarch64-unknown-linux-gnu",
        "aarch64-unknown-none-softfloat",
    ),
    ("aarch64-unknown-none", "aarch64-unknown-none-softfloat"),
    (
        "loongarch64-unknown-none",
        "loongarch64-unknown-none-softfloat",
    ),
];

pub(crate) fn run_workspace_clippy_command(args: &crate::ClippyArgs) -> anyhow::Result<()> {
    validate_clippy_args(args)?;
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = if args.since.is_some() && args.packages.is_empty() && !args.all {
        crate::context::workspace_metadata_root_manifest_with_deps(&workspace_manifest)
    } else {
        crate::context::workspace_metadata_root_manifest(&workspace_manifest)
    }
    .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let all_packages = workspace_packages(&metadata);
    let packages = skip_unsupported_packages(resolve_requested_packages(
        args,
        &workspace_root,
        &metadata,
        &all_packages,
    )?);
    if packages.is_empty() {
        println!(
            "no clippy packages selected from {}; skipping",
            workspace_root.display()
        );
        return Ok(());
    }
    let checks = expand_clippy_checks(&packages, &metadata)?;

    println!(
        "running clippy for {} package(s) with {} check(s) from {}",
        packages.len(),
        checks.len(),
        workspace_root.display()
    );

    let mut runner = ProcessCargoRunner;
    let report = run_clippy_checks(&mut runner, &workspace_root, &checks)?;
    print_report_summary(&report);

    if report.failed_packages().is_empty() {
        println!("all clippy checks passed");
        return Ok(());
    }

    bail!(
        "clippy failed for {} package(s): {}",
        report.failed_packages().len(),
        report.failed_packages().join(", ")
    )
}

fn validate_clippy_args(args: &crate::ClippyArgs) -> anyhow::Result<()> {
    if args.since.is_some() && !args.packages.is_empty() {
        bail!("`--since` cannot be combined with `--package`; choose one package selection mode");
    }
    if args.since.is_some() && args.all {
        bail!("`--since` cannot be combined with `--all`; choose one package selection mode");
    }
    Ok(())
}

fn workspace_packages(metadata: &Metadata) -> Vec<Package> {
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

fn resolve_requested_packages(
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

    let package_names = if !args.packages.is_empty() {
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
            crate::support::git::IncrementalPackageSelection::Packages(packages) => {
                println!(
                    "incremental clippy since `{since}` selected {} package(s)",
                    packages.len()
                );
                packages
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

    package_names
        .into_iter()
        .map(|package| {
            package_lookup
                .get(package.as_str())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))
        })
        .collect()
}

fn validate_requested_packages(
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ClippyCheckKind {
    Base,
    Feature(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClippyCheck {
    package: String,
    kind: ClippyCheckKind,
    target: Option<String>,
    env: Vec<(String, String)>,
    axconfig_override: Option<ClippyAxconfigOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClippyAxconfigOverride {
    target: String,
    platform_config: PathBuf,
    out_config: PathBuf,
    overrides: Vec<String>,
}

impl ClippyAxconfigOverride {
    fn generate(&self, workspace_root: &Path) -> anyhow::Result<()> {
        let platform_name =
            read_config_string(std::slice::from_ref(&self.platform_config), "platform")
                .with_context(|| {
                    format!(
                        "failed to read platform name from {}",
                        self.platform_config.display()
                    )
                })?;

        crate::build::generate_axconfig(
            workspace_root,
            &self.target,
            &platform_name,
            &self.platform_config,
            &self.out_config,
            None,
            &self.overrides,
        )
        .with_context(|| {
            format!(
                "failed to generate clippy axconfig override at {}",
                self.out_config.display()
            )
        })
    }
}

impl ClippyCheck {
    fn cargo_args(&self) -> Vec<String> {
        let mut args = match &self.kind {
            ClippyCheckKind::Base => vec!["clippy".into(), "-p".into(), self.package.clone()],
            ClippyCheckKind::Feature(feature) => vec![
                "clippy".into(),
                "-p".into(),
                self.package.clone(),
                "--no-default-features".into(),
                "--features".into(),
                feature.clone(),
            ],
        };
        if let Some(target) = &self.target {
            args.extend(["--target".into(), target.clone()]);
        }
        args.extend(["--".into(), "-D".into(), "warnings".into()]);
        args
    }

    fn label(&self) -> String {
        let base = match &self.kind {
            ClippyCheckKind::Base => format!("{} (base", self.package),
            ClippyCheckKind::Feature(feature) => {
                format!("{} (feature: {}", self.package, feature)
            }
        };

        match &self.target {
            Some(target) => format!("{base}, target: {target})"),
            None => format!("{base})"),
        }
    }

    fn env_prefix(&self) -> String {
        self.env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn docs_rs_targets(package: &Package) -> Vec<String> {
    let Some(docs_rs) = package
        .metadata
        .get(DOCS_RS_METADATA)
        .and_then(Value::as_object)
        .or_else(|| {
            package
                .metadata
                .get(DOCS_METADATA)
                .and_then(Value::as_object)
                .and_then(|docs| docs.get(RS_METADATA))
                .and_then(Value::as_object)
        })
    else {
        return Vec::new();
    };

    let Some(targets) = docs_rs.get(TARGETS_METADATA).and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut unique_targets = BTreeSet::new();
    for target in targets.iter().filter_map(Value::as_str) {
        unique_targets.insert(normalize_clippy_target(target).to_string());
    }

    unique_targets.into_iter().collect()
}

fn normalize_clippy_target(target: &str) -> &str {
    CLIPPY_TARGET_ALIASES
        .iter()
        .find_map(|(source, normalized)| (*source == target).then_some(*normalized))
        .unwrap_or(target)
}

fn clippy_skip_reason(package: &Package) -> Option<&str> {
    UNSUPPORTED_CLIPPY_PACKAGES
        .iter()
        .find_map(|(name, reason)| (package.name == *name).then_some(*reason))
}

fn skip_unsupported_packages(packages: Vec<Package>) -> Vec<Package> {
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

fn clippy_env(package: &Package, metadata: &Metadata) -> anyhow::Result<Vec<(String, String)>> {
    if package.name == ARCEOS_RUST_PACKAGE {
        return arceos_rust_clippy_env(metadata);
    }

    let Some(manifest_dir) = package.manifest_path.parent() else {
        return Ok(Vec::new());
    };
    let axconfig = manifest_dir.join(AXCONFIG_FILE);
    if !axconfig.exists() {
        return Ok(Vec::new());
    }

    Ok(vec![(AX_CONFIG_PATH_ENV.to_string(), axconfig.to_string())])
}

fn package_axconfig_path(package: &Package) -> Option<PathBuf> {
    let manifest_dir = package.manifest_path.parent()?;
    let axconfig = manifest_dir.join(AXCONFIG_FILE);
    axconfig.exists().then(|| axconfig.into_std_path_buf())
}

fn feature_axconfig_overrides(package: &Package) -> HashMap<String, Vec<String>> {
    let Some(overrides) = package
        .metadata
        .get(AXBUILD_METADATA)
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get(CLIPPY_FEATURE_AXCONFIG_OVERRIDES_METADATA))
        .and_then(Value::as_object)
    else {
        return HashMap::new();
    };

    overrides
        .iter()
        .filter_map(|(feature, values)| {
            let values = values
                .as_array()?
                .iter()
                .map(Value::as_str)
                .map(Option::unwrap_or_default)
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            Some((feature.clone(), values))
        })
        .collect()
}

fn clippy_axconfig_override(
    package: &Package,
    target: Option<&str>,
    feature: &str,
    overrides: &[String],
    workspace_root: &Path,
) -> Option<ClippyAxconfigOverride> {
    if overrides.is_empty() {
        return None;
    }
    let target = target?.to_string();
    let platform_config = package_axconfig_path(package)?;
    let out_config = crate::context::axbuild_tmp_dir(workspace_root)
        .join("axconfig")
        .join(package.name.as_str())
        .join(target.as_str())
        .join("clippy")
        .join(feature)
        .join(".axconfig.toml");

    Some(ClippyAxconfigOverride {
        target,
        platform_config,
        out_config,
        overrides: overrides.to_vec(),
    })
}

fn with_axconfig_env_override(
    mut env: Vec<(String, String)>,
    override_config: Option<&ClippyAxconfigOverride>,
) -> Vec<(String, String)> {
    let Some(override_config) = override_config else {
        return env;
    };
    env.retain(|(key, _)| key != AX_CONFIG_PATH_ENV);
    env.push((
        AX_CONFIG_PATH_ENV.to_string(),
        override_config.out_config.display().to_string(),
    ));
    env
}

fn arceos_rust_clippy_env(metadata: &Metadata) -> anyhow::Result<Vec<(String, String)>> {
    let mut envs = HashMap::new();
    crate::build::prepare_std_build_env(&mut envs, ARCEOS_RUST_CLIPPY_TARGET, metadata)
        .context("failed to prepare arceos-rust clippy config")?;
    Ok(envs.into_iter().collect())
}

fn expand_clippy_checks(
    packages: &[Package],
    metadata: &Metadata,
) -> anyhow::Result<Vec<ClippyCheck>> {
    let mut checks = Vec::new();
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();

    for package in packages {
        let features: BTreeSet<_> = package
            .features
            .keys()
            .filter(|feature| feature.as_str() != DEFAULT_FEATURE)
            .cloned()
            .collect();
        let targets = docs_rs_targets(package);
        let target_iter = if targets.is_empty() {
            vec![None]
        } else {
            targets.into_iter().map(Some).collect()
        };
        let env = clippy_env(package, metadata)
            .with_context(|| format!("failed to prepare clippy env for `{}`", package.name))?;
        let axconfig_overrides = feature_axconfig_overrides(package);

        for target in target_iter {
            checks.push(ClippyCheck {
                package: package.name.to_string(),
                kind: ClippyCheckKind::Base,
                target: target.clone(),
                env: env.clone(),
                axconfig_override: None,
            });

            for feature in &features {
                let axconfig_override = axconfig_overrides.get(feature).and_then(|overrides| {
                    clippy_axconfig_override(
                        package,
                        target.as_deref(),
                        feature,
                        overrides,
                        &workspace_root,
                    )
                });
                checks.push(ClippyCheck {
                    package: package.name.to_string(),
                    kind: ClippyCheckKind::Feature(feature.clone()),
                    target: target.clone(),
                    env: with_axconfig_env_override(env.clone(), axconfig_override.as_ref()),
                    axconfig_override,
                });
            }
        }
    }

    Ok(checks)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageRunReport {
    package: String,
    total_checks: usize,
    failed_checks: Vec<String>,
}

impl PackageRunReport {
    fn new(package: String) -> Self {
        Self {
            package,
            total_checks: 0,
            failed_checks: Vec::new(),
        }
    }

    fn passed(&self) -> bool {
        self.failed_checks.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClippyRunReport {
    total_checks: usize,
    passed_checks: usize,
    packages: Vec<PackageRunReport>,
}

impl ClippyRunReport {
    fn passed_packages(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|package| package.passed())
            .map(|package| package.package.clone())
            .collect()
    }

    fn failed_packages(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|package| !package.passed())
            .map(|package| package.package.clone())
            .collect()
    }
}

fn run_clippy_checks<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    checks: &[ClippyCheck],
) -> anyhow::Result<ClippyRunReport> {
    let mut packages = Vec::new();
    let mut package_indexes = HashMap::new();

    for check in checks {
        if package_indexes.contains_key(check.package.as_str()) {
            continue;
        }
        let index = packages.len();
        packages.push(PackageRunReport::new(check.package.clone()));
        package_indexes.insert(check.package.clone(), index);
    }

    let mut passed_checks = 0;

    for (index, check) in checks.iter().enumerate() {
        let args = check.cargo_args();
        println!("[{}/{}] {}", index + 1, checks.len(), check.label());
        if check.env.is_empty() {
            println!("          cargo {}", args.join(" "));
        } else {
            println!("          {} cargo {}", check.env_prefix(), args.join(" "));
        }

        let success = runner.run_clippy(workspace_root, check)?;
        let package_index = package_indexes[check.package.as_str()];
        let package_report = &mut packages[package_index];
        package_report.total_checks += 1;

        if success {
            passed_checks += 1;
            println!("ok: {}", check.label());
        } else {
            package_report.failed_checks.push(check.label());
            bail!(
                "clippy failed for {}: aborting (fail-fast, {} check(s) remaining)",
                check.label(),
                checks.len() - index - 1
            );
        }
    }

    Ok(ClippyRunReport {
        total_checks: checks.len(),
        passed_checks,
        packages,
    })
}

fn print_report_summary(report: &ClippyRunReport) {
    println!(
        "clippy summary: {} package(s), {} check(s), {} package(s) passed, {} package(s) failed",
        report.packages.len(),
        report.total_checks,
        report.passed_packages().len(),
        report.failed_packages().len()
    );
    println!(
        "passed checks: {}, failed checks: {}",
        report.passed_checks,
        report.total_checks.saturating_sub(report.passed_checks)
    );

    let failed_packages = report.failed_packages();
    if !failed_packages.is_empty() {
        eprintln!("failed packages: {}", failed_packages.join(", "));
        for package in report.packages.iter().filter(|package| !package.passed()) {
            eprintln!(
                "  {} failed {} check(s): {}",
                package.package,
                package.failed_checks.len(),
                package.failed_checks.join(", ")
            );
        }
    }
}

trait CargoRunner {
    fn run_clippy(&mut self, workspace_root: &Path, check: &ClippyCheck) -> anyhow::Result<bool>;
}

struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run_clippy(&mut self, workspace_root: &Path, check: &ClippyCheck) -> anyhow::Result<bool> {
        if let Some(axconfig_override) = &check.axconfig_override {
            axconfig_override.generate(workspace_root)?;
        }
        let args = check.cargo_args();
        run_cargo_status_with_env(workspace_root, &args, &check.env)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::*;

    fn pkg(
        name: &str,
        id: &str,
        features: &[(&str, &[&str])],
        docs_rs_targets: Option<&[&str]>,
    ) -> Package {
        let metadata = docs_rs_targets.map(|targets| {
            serde_json::json!({
                "docs.rs": {
                    "targets": targets,
                }
            })
        });
        let value = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "id": id,
            "license": null,
            "license_file": null,
            "description": null,
            "source": null,
            "dependencies": [],
            "targets": [{
                "kind": ["lib"],
                "crate_types": ["lib"],
                "name": name,
                "src_path": format!("/tmp/{name}/src/lib.rs"),
                "edition": "2021",
                "doc": true,
                "doctest": true,
                "test": true
            }],
            "features": features.iter().map(|(k, v)| ((*k).to_string(), v.iter().map(|item| (*item).to_string()).collect::<Vec<_>>())).collect::<HashMap<_, _>>(),
            "manifest_path": format!("/tmp/{name}/Cargo.toml"),
            "metadata": metadata,
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
        });

        serde_json::from_value(value).unwrap()
    }

    fn pkg_with_metadata(
        name: &str,
        id: &str,
        features: &[(&str, &[&str])],
        metadata: Value,
    ) -> Package {
        let value = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "id": id,
            "license": null,
            "license_file": null,
            "description": null,
            "source": null,
            "dependencies": [],
            "targets": [{
                "kind": ["lib"],
                "crate_types": ["lib"],
                "name": name,
                "src_path": format!("/tmp/{name}/src/lib.rs"),
                "edition": "2021",
                "doc": true,
                "doctest": true,
                "test": true
            }],
            "features": features.iter().map(|(k, v)| ((*k).to_string(), v.iter().map(|item| (*item).to_string()).collect::<Vec<_>>())).collect::<HashMap<_, _>>(),
            "manifest_path": format!("/tmp/{name}/Cargo.toml"),
            "metadata": metadata,
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
        });

        serde_json::from_value(value).unwrap()
    }

    fn pkg_with_manifest_path(
        name: &str,
        id: &str,
        features: &[(&str, &[&str])],
        docs_rs_targets: Option<&[&str]>,
        manifest_path: PathBuf,
    ) -> Package {
        let metadata = docs_rs_targets.map(|targets| {
            serde_json::json!({
                "docs.rs": {
                    "targets": targets,
                }
            })
        });
        let value = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "id": id,
            "license": null,
            "license_file": null,
            "description": null,
            "source": null,
            "dependencies": [],
            "targets": [{
                "kind": ["lib"],
                "crate_types": ["lib"],
                "name": name,
                "src_path": manifest_path.parent().unwrap().join("src/lib.rs"),
                "edition": "2021",
                "doc": true,
                "doctest": true,
                "test": true
            }],
            "features": features.iter().map(|(k, v)| ((*k).to_string(), v.iter().map(|item| (*item).to_string()).collect::<Vec<_>>())).collect::<HashMap<_, _>>(),
            "manifest_path": manifest_path,
            "metadata": metadata,
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
        });

        serde_json::from_value(value).unwrap()
    }

    fn pkg_with_manifest_path_and_metadata(
        name: &str,
        id: &str,
        features: &[(&str, &[&str])],
        docs_rs_targets: Option<&[&str]>,
        manifest_path: PathBuf,
        package_metadata: Value,
    ) -> Package {
        let mut metadata = docs_rs_targets
            .map(|targets| {
                serde_json::json!({
                    "docs.rs": {
                        "targets": targets,
                    }
                })
            })
            .unwrap_or_else(|| serde_json::json!({}));
        if let (Some(dst), Some(src)) = (metadata.as_object_mut(), package_metadata.as_object()) {
            dst.extend(src.clone());
        }

        let value = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "id": id,
            "license": null,
            "license_file": null,
            "description": null,
            "source": null,
            "dependencies": [],
            "targets": [{
                "kind": ["lib"],
                "crate_types": ["lib"],
                "name": name,
                "src_path": manifest_path.parent().unwrap().join("src/lib.rs"),
                "edition": "2021",
                "doc": true,
                "doctest": true,
                "test": true
            }],
            "features": features.iter().map(|(k, v)| ((*k).to_string(), v.iter().map(|item| (*item).to_string()).collect::<Vec<_>>())).collect::<HashMap<_, _>>(),
            "manifest_path": manifest_path,
            "metadata": metadata,
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
        });

        serde_json::from_value(value).unwrap()
    }

    fn metadata_with_packages(packages: Vec<Package>, workspace_members: &[&str]) -> Metadata {
        let package_refs = packages;
        let value = serde_json::json!({
            "packages": package_refs,
            "workspace_members": workspace_members,
            "workspace_default_members": workspace_members,
            "resolve": null,
            "target_directory": "/tmp/target",
            "version": 1,
            "workspace_root": "/tmp/ws",
            "metadata": null,
        });

        serde_json::from_value(value).unwrap()
    }

    fn metadata_for_packages(packages: &[Package]) -> Metadata {
        let members = packages
            .iter()
            .map(|package| package.id.repr.as_str())
            .collect::<Vec<_>>();
        metadata_with_packages(packages.to_vec(), &members)
    }

    fn expand(packages: &[Package]) -> Vec<ClippyCheck> {
        expand_clippy_checks(packages, &metadata_for_packages(packages))
            .expect("test package clippy checks should expand")
    }

    fn args(all: bool, packages: &[&str]) -> crate::ClippyArgs {
        crate::ClippyArgs {
            all,
            packages: packages
                .iter()
                .map(|package| (*package).to_string())
                .collect(),
            since: None,
        }
    }

    struct FakeCargoRunner {
        results: HashMap<ClippyCheck, bool>,
        invocations: Vec<(PathBuf, ClippyCheck)>,
    }

    impl FakeCargoRunner {
        fn new(results: &[(ClippyCheck, bool)]) -> Self {
            Self {
                results: results.iter().cloned().collect(),
                invocations: Vec::new(),
            }
        }
    }

    impl CargoRunner for FakeCargoRunner {
        fn run_clippy(
            &mut self,
            workspace_root: &Path,
            check: &ClippyCheck,
        ) -> anyhow::Result<bool> {
            self.invocations
                .push((workspace_root.to_path_buf(), check.clone()));
            Ok(*self.results.get(check).unwrap_or(&true))
        }
    }

    #[test]
    fn workspace_package_extraction_keeps_only_workspace_members() {
        let metadata = metadata_with_packages(
            vec![
                pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
                pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
                pkg("gamma", "gamma 0.1.0 (path+file:///tmp/gamma)", &[], None),
            ],
            &[
                "beta 0.1.0 (path+file:///tmp/beta)",
                "alpha 0.1.0 (path+file:///tmp/alpha)",
            ],
        );

        let packages = workspace_packages(&metadata);

        assert_eq!(
            packages
                .iter()
                .map(|pkg| pkg.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }

    fn known_packages() -> HashSet<&'static str> {
        HashSet::from(["alpha", "beta", "gamma"])
    }

    #[test]
    fn default_mode_selects_every_workspace_package() {
        let packages = vec![
            pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
            pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
        ];
        let metadata = metadata_with_packages(
            packages.clone(),
            &[
                "alpha 0.1.0 (path+file:///tmp/alpha)",
                "beta 0.1.0 (path+file:///tmp/beta)",
            ],
        );
        let resolved = resolve_requested_packages(
            &args(false, &[]),
            Path::new("/tmp/ws"),
            &metadata,
            &packages,
        )
        .unwrap();

        assert_eq!(
            resolved
                .iter()
                .map(|pkg| pkg.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn package_selection_overrides_default_workspace_selection() {
        let packages = vec![
            pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
            pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
        ];
        let metadata = metadata_with_packages(
            packages.clone(),
            &[
                "alpha 0.1.0 (path+file:///tmp/alpha)",
                "beta 0.1.0 (path+file:///tmp/beta)",
            ],
        );
        let resolved = resolve_requested_packages(
            &args(false, &["beta"]),
            Path::new("/tmp/ws"),
            &metadata,
            &packages,
        )
        .unwrap();

        assert_eq!(
            resolved
                .iter()
                .map(|pkg| pkg.name.as_str())
                .collect::<Vec<_>>(),
            vec!["beta"]
        );
    }

    #[test]
    fn duplicate_explicit_packages_are_rejected() {
        let known = known_packages();
        let err =
            validate_requested_packages(&["alpha".into(), "alpha".into()], &known).unwrap_err();

        assert!(
            err.to_string()
                .contains("duplicate workspace package `alpha`")
        );
    }

    #[test]
    fn since_rejects_explicit_package_selection() {
        let mut args = args(false, &["alpha"]);
        args.since = Some("origin/main".to_string());

        let err = validate_clippy_args(&args).unwrap_err();

        assert!(
            err.to_string()
                .contains("cannot be combined with `--package`")
        );
    }

    #[test]
    fn since_rejects_all_selection() {
        let mut args = args(true, &[]);
        args.since = Some("origin/main".to_string());

        let err = validate_clippy_args(&args).unwrap_err();

        assert!(err.to_string().contains("cannot be combined with `--all`"));
    }

    #[test]
    fn feature_expansion_ignores_default() {
        let packages = vec![pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("default", &["feat-a"]), ("feat-b", &[]), ("feat-a", &[])],
            None,
        )];

        let checks = expand(&packages);

        assert_eq!(
            checks,
            vec![
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Base,
                    target: None,
                    env: Vec::new(),
                    axconfig_override: None,
                },
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Feature("feat-a".into()),
                    target: None,
                    env: Vec::new(),
                    axconfig_override: None,
                },
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Feature("feat-b".into()),
                    target: None,
                    env: Vec::new(),
                    axconfig_override: None,
                },
            ]
        );
    }

    #[test]
    fn feature_expansion_is_deterministic() {
        let packages = vec![
            pkg(
                "beta",
                "beta 0.1.0 (path+file:///tmp/beta)",
                &[("zeta", &[]), ("alpha", &[])],
                None,
            ),
            pkg(
                "alpha",
                "alpha 0.1.0 (path+file:///tmp/alpha)",
                &[("middle", &[]), ("default", &[])],
                None,
            ),
        ];

        let checks = expand(&packages);

        assert_eq!(
            checks
                .into_iter()
                .map(|check| check.label())
                .collect::<Vec<_>>(),
            vec![
                "beta (base)",
                "beta (feature: alpha)",
                "beta (feature: zeta)",
                "alpha (base)",
                "alpha (feature: middle)",
            ]
        );
    }

    #[test]
    fn package_without_features_yields_only_base_check() {
        let checks = expand(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[],
            None,
        )]);

        assert_eq!(
            checks,
            vec![ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Base,
                target: None,
                env: Vec::new(),
                axconfig_override: None,
            }]
        );
    }

    #[test]
    fn package_with_features_yields_base_plus_each_feature() {
        let checks = expand(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("b", &[]), ("a", &[])],
            None,
        )]);

        assert_eq!(checks.len(), 3);
        assert_eq!(
            checks[0].cargo_args(),
            vec!["clippy", "-p", "alpha", "--", "-D", "warnings"]
        );
        assert_eq!(
            checks[1].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--no-default-features",
                "--features",
                "a",
                "--",
                "-D",
                "warnings",
            ]
        );
        assert_eq!(
            checks[2].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--no-default-features",
                "--features",
                "b",
                "--",
                "-D",
                "warnings",
            ]
        );
    }

    #[test]
    fn docs_rs_targets_expand_base_and_feature_checks() {
        let checks = expand(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("b", &[]), ("a", &[])],
            Some(&["riscv64gc-unknown-none-elf"]),
        )]);

        assert_eq!(checks.len(), 3);
        assert_eq!(
            checks[0].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--target",
                "riscv64gc-unknown-none-elf",
                "--",
                "-D",
                "warnings",
            ]
        );
        assert_eq!(
            checks[1].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--no-default-features",
                "--features",
                "a",
                "--target",
                "riscv64gc-unknown-none-elf",
                "--",
                "-D",
                "warnings",
            ]
        );
        assert_eq!(
            checks[2].label(),
            "alpha (feature: b, target: riscv64gc-unknown-none-elf)"
        );
    }

    #[test]
    fn nested_docs_rs_targets_expand_base_checks() {
        let checks = expand(&[pkg_with_metadata(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[],
            serde_json::json!({
                "docs": {
                    "rs": {
                        "targets": ["aarch64-unknown-none"],
                    },
                },
            }),
        )]);

        assert_eq!(
            checks[0].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--target",
                "aarch64-unknown-none-softfloat",
                "--",
                "-D",
                "warnings",
            ]
        );
    }

    #[test]
    fn docs_rs_targets_are_normalized_to_workspace_toolchain_targets() {
        let checks = expand(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[],
            Some(&["loongarch64-unknown-none"]),
        )]);

        assert_eq!(
            checks[0].label(),
            "alpha (base, target: loongarch64-unknown-none-softfloat)"
        );
    }

    #[test]
    fn docs_rs_targets_are_sorted_and_deduplicated() {
        let checks = expand(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("feat", &[])],
            Some(&[
                "x86_64-unknown-none",
                "aarch64-unknown-none-softfloat",
                "x86_64-unknown-none",
            ]),
        )]);

        assert_eq!(
            checks
                .into_iter()
                .map(|check| check.label())
                .collect::<Vec<_>>(),
            vec![
                "alpha (base, target: aarch64-unknown-none-softfloat)",
                "alpha (feature: feat, target: aarch64-unknown-none-softfloat)",
                "alpha (base, target: x86_64-unknown-none)",
                "alpha (feature: feat, target: x86_64-unknown-none)",
            ]
        );
    }

    #[test]
    fn empty_docs_rs_targets_fall_back_to_host_clippy() {
        let package = pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[],
            Some(&[]),
        );

        assert!(docs_rs_targets(&package).is_empty());
        assert_eq!(
            expand(&[package])[0].cargo_args(),
            vec!["clippy", "-p", "alpha", "--", "-D", "warnings"]
        );
    }

    #[test]
    fn unsupported_packages_can_skip_generic_clippy() {
        let packages = vec![
            pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
            pkg(
                "axvisor",
                "axvisor 0.1.0 (path+file:///tmp/axvisor)",
                &[],
                None,
            ),
        ];

        let filtered = skip_unsupported_packages(packages);

        assert_eq!(
            filtered
                .iter()
                .map(|package| package.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha"]
        );
    }

    #[test]
    fn package_axconfig_is_passed_as_clippy_env() {
        let temp = tempfile::tempdir().unwrap();
        let package_dir = temp.path().join("alpha");
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::write(package_dir.join("axconfig.toml"), "").unwrap();

        let package = pkg_with_manifest_path(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[],
            None,
            package_dir.join("Cargo.toml"),
        );

        let checks = expand(&[package]);

        assert_eq!(
            checks[0].env,
            vec![(
                "AX_CONFIG_PATH".to_string(),
                package_dir.join("axconfig.toml").display().to_string(),
            )]
        );
    }

    #[test]
    fn feature_axconfig_overrides_apply_only_to_that_feature_check() {
        let temp = tempfile::tempdir().unwrap();
        let package_dir = temp.path().join("alpha");
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::write(package_dir.join("axconfig.toml"), "").unwrap();

        let package = pkg_with_manifest_path_and_metadata(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("cntv-timer", &[]), ("gic-v3", &[])],
            Some(&["aarch64-unknown-none"]),
            package_dir.join("Cargo.toml"),
            serde_json::json!({
                "axbuild": {
                    "clippy-feature-axconfig-overrides": {
                        "cntv-timer": ["devices.timer-irq=27"]
                    }
                }
            }),
        );

        let checks = expand(&[package]);

        assert_eq!(
            checks
                .iter()
                .map(|check| (check.label(), check.env.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "alpha (base, target: aarch64-unknown-none-softfloat)".to_string(),
                    vec![(
                        "AX_CONFIG_PATH".to_string(),
                        package_dir.join("axconfig.toml").display().to_string(),
                    )],
                ),
                (
                    "alpha (feature: cntv-timer, target: aarch64-unknown-none-softfloat)"
                        .to_string(),
                    vec![(
                        "AX_CONFIG_PATH".to_string(),
                        "/tmp/ws/tmp/axbuild/axconfig/alpha/aarch64-unknown-none-softfloat/clippy/\
                         cntv-timer/.axconfig.toml"
                            .to_string(),
                    )],
                ),
                (
                    "alpha (feature: gic-v3, target: aarch64-unknown-none-softfloat)".to_string(),
                    vec![(
                        "AX_CONFIG_PATH".to_string(),
                        package_dir.join("axconfig.toml").display().to_string(),
                    )],
                ),
            ]
        );
    }

    #[test]
    fn arceos_rust_config_is_passed_as_clippy_env() {
        const ARCEOS_RUST_CONFIG_ENV: &str = "ARCEOS_RUST_CONFIG";

        let metadata = crate::build::workspace_metadata().unwrap();
        let package = metadata
            .packages
            .iter()
            .find(|package| package.name == ARCEOS_RUST_PACKAGE)
            .cloned()
            .expect("arceos-rust package should be in workspace metadata");

        let checks = expand_clippy_checks(&[package], &metadata).unwrap();

        assert!(
            checks[0].env.iter().any(|(key, value)| {
                key == ARCEOS_RUST_CONFIG_ENV
                    && value.ends_with(
                        "tmp/axbuild/axconfig/arceos-rust/x86_64-unknown-none/.axconfig.toml",
                    )
            }),
            "expected {ARCEOS_RUST_CONFIG_ENV} in {:?}",
            checks[0].env
        );
    }

    #[test]
    fn package_failures_abort_remaining_checks() {
        let root = PathBuf::from("/tmp/workspace");
        let checks = vec![
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Base,
                target: None,
                env: Vec::new(),
                axconfig_override: None,
            },
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Feature("feat-a".into()),
                target: None,
                env: Vec::new(),
                axconfig_override: None,
            },
            ClippyCheck {
                package: "beta".into(),
                kind: ClippyCheckKind::Base,
                target: None,
                env: Vec::new(),
                axconfig_override: None,
            },
        ];
        let mut runner = FakeCargoRunner::new(&[
            (checks[0].clone(), true),
            (checks[1].clone(), false),
            (checks[2].clone(), true),
        ]);

        let err = run_clippy_checks(&mut runner, &root, &checks).unwrap_err();

        assert_eq!(
            err.to_string(),
            "clippy failed for alpha (feature: feat-a): aborting (fail-fast, 1 check(s) remaining)"
        );
        assert_eq!(
            runner.invocations,
            vec![
                (root.clone(), checks[0].clone()),
                (root.clone(), checks[1].clone()),
            ]
        );
    }

    #[test]
    fn report_tracks_passing_and_failing_packages_for_mixed_runs() {
        let report = ClippyRunReport {
            total_checks: 3,
            passed_checks: 2,
            packages: vec![
                PackageRunReport {
                    package: "alpha".into(),
                    total_checks: 2,
                    failed_checks: vec!["alpha (feature: feat-a)".into()],
                },
                PackageRunReport {
                    package: "beta".into(),
                    total_checks: 1,
                    failed_checks: Vec::new(),
                },
            ],
        };

        assert_eq!(report.failed_packages(), vec!["alpha"]);
        assert_eq!(report.passed_packages(), vec!["beta"]);
    }
}
