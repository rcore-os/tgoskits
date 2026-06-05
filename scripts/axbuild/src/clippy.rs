use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use ax_config_gen::read_config_string;
use cargo_metadata::{Metadata, Package};
use chrono::Local;
use serde_json::Value;

use crate::support::process::run_cargo_status_with_env;

const DEFAULT_FEATURE: &str = "default";
const AX_CONFIG_PATH_ENV: &str = "AX_CONFIG_PATH";
const AXCONFIG_FILE: &str = "axconfig.toml";
const AXSTD_STD_PACKAGE: &str = "ax-std";
const AXSTD_STD_DEFAULT_FEATURE: &str = "default";
const AXSTD_STD_CLIPPY_FEATURES: &str = "std-compat,x86-pc,fs,multitask,irq,net";
const AXSTD_STD_CLIPPY_TARGET: &str = "x86_64-unknown-none";
const DOCS_RS_METADATA: &str = "docs.rs";
const DOCS_METADATA: &str = "docs";
const RS_METADATA: &str = "rs";
const TARGETS_METADATA: &str = "targets";
const AXBUILD_METADATA: &str = "axbuild";
const CLIPPY_FEATURE_AXCONFIG_OVERRIDES_METADATA: &str = "clippy-feature-axconfig-overrides";
const AX_HAL_PACKAGE: &str = "ax-hal";

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

const AX_HAL_PLATFORM_FEATURE_TARGET_ARCHES: &[(&str, &[&str])] = &[
    ("plat-dyn", &["aarch64", "loongarch64", "riscv64", "x86_64"]),
    ("loongarch64-qemu-virt", &["loongarch64"]),
    ("riscv64-sg2002", &["riscv64"]),
    ("riscv64-visionfive2", &["riscv64"]),
    ("x86-pc", &["x86_64"]),
    ("x86-qemu-q35", &["x86_64"]),
];

pub(crate) fn run_workspace_clippy_command(args: &crate::ClippyArgs) -> anyhow::Result<()> {
    validate_clippy_args(args)?;
    let started_at = Local::now();
    let timer = Instant::now();
    println!(
        "clippy started at: {}",
        started_at.format("%Y-%m-%d %H:%M:%S %z")
    );
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = if clippy_metadata_needs_deps(args) {
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
        print_clippy_timing(timer.elapsed());
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
    let report = match run_clippy_checks(&mut runner, &workspace_root, &checks) {
        Ok(report) => report,
        Err(err) => {
            print_clippy_timing(timer.elapsed());
            return Err(err);
        }
    };
    print_report_summary(&report);
    print_clippy_timing(timer.elapsed());

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

fn print_clippy_timing(elapsed: Duration) {
    let finished_at = Local::now();
    println!(
        "clippy finished at: {}",
        finished_at.format("%Y-%m-%d %H:%M:%S %z")
    );
    println!("clippy elapsed: {}", format_elapsed(elapsed));
}

fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let millis = elapsed.subsec_millis();
    if secs == 0 {
        return format!("{}ms", millis);
    }

    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

fn clippy_metadata_needs_deps(args: &crate::ClippyArgs) -> bool {
    args.since.is_some() && args.packages.is_empty() && !args.all
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

#[derive(Debug, Clone)]
struct SelectedClippyPackage {
    package: Package,
    deps_mode: ClippyDepsMode,
}

fn resolve_requested_packages(
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
fn incremental_clippy_selections(
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
enum ClippyDepsMode {
    NoDeps,
    WithDeps,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClippyCheck {
    package: String,
    kind: ClippyCheckKind,
    deps_mode: ClippyDepsMode,
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
        if matches!(self.deps_mode, ClippyDepsMode::NoDeps) {
            args.insert(1, "--no-deps".into());
        }
        if self.package == AXSTD_STD_PACKAGE
            && matches!(&self.kind, ClippyCheckKind::Feature(feature) if feature == AXSTD_STD_DEFAULT_FEATURE)
        {
            args = vec![
                "clippy".into(),
                "-p".into(),
                self.package.clone(),
                "--no-default-features".into(),
                "--features".into(),
                AXSTD_STD_CLIPPY_FEATURES.into(),
            ];
        }
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

fn clippy_target_arch(target: &str) -> Option<&'static str> {
    if target.starts_with("aarch64-") {
        Some("aarch64")
    } else if target.starts_with("loongarch64-") {
        Some("loongarch64")
    } else if target.starts_with("riscv64") {
        Some("riscv64")
    } else if target.starts_with("x86_64-") {
        Some("x86_64")
    } else {
        None
    }
}

fn ax_hal_platform_target_arches(feature: &str) -> Option<&'static [&'static str]> {
    AX_HAL_PLATFORM_FEATURE_TARGET_ARCHES
        .iter()
        .find_map(|(platform_feature, target_arches)| {
            (*platform_feature == feature).then_some(*target_arches)
        })
}

fn ax_hal_feature_dependency(feature_dependency: &str) -> Option<&str> {
    feature_dependency
        .strip_prefix("ax-hal/")
        .or_else(|| feature_dependency.strip_prefix("ax-hal?/"))
}

fn ax_hal_platform_constraints<'a>(
    package: &'a Package,
    feature: &'a str,
) -> Vec<&'static [&'static str]> {
    let mut constraints = Vec::new();
    if package.name == AX_HAL_PACKAGE
        && let Some(target_arches) = ax_hal_platform_target_arches(feature)
    {
        constraints.push(target_arches);
    }

    if let Some(feature_dependencies) = package.features.get(feature) {
        constraints.extend(
            feature_dependencies
                .iter()
                .filter_map(|feature_dependency| ax_hal_feature_dependency(feature_dependency))
                .filter_map(ax_hal_platform_target_arches),
        );
    }

    constraints
}

fn feature_supported_on_clippy_target(
    package: &Package,
    feature: &str,
    target: Option<&str>,
) -> bool {
    let constraints = ax_hal_platform_constraints(package, feature);
    if constraints.is_empty() {
        return true;
    }
    let Some(target) = target else {
        return false;
    };
    clippy_target_arch(target).is_some_and(|arch| {
        constraints
            .iter()
            .all(|target_arches| target_arches.contains(&arch))
    })
}

fn clippy_skip_reason(package: &Package) -> Option<&str> {
    UNSUPPORTED_CLIPPY_PACKAGES
        .iter()
        .find_map(|(name, reason)| (package.name == *name).then_some(*reason))
}

fn skip_unsupported_packages(packages: Vec<SelectedClippyPackage>) -> Vec<SelectedClippyPackage> {
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

fn clippy_env(package: &Package) -> Vec<(String, String)> {
    let Some(manifest_dir) = package.manifest_path.parent() else {
        return Vec::new();
    };
    let axconfig = manifest_dir.join(AXCONFIG_FILE);
    if !axconfig.exists() {
        return Vec::new();
    }

    vec![(AX_CONFIG_PATH_ENV.to_string(), axconfig.to_string())]
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

fn axstd_std_clippy_env(metadata: &Metadata) -> anyhow::Result<Vec<(String, String)>> {
    let mut envs = HashMap::new();
    crate::build::prepare_std_build_env(&mut envs, AXSTD_STD_CLIPPY_TARGET, false, metadata)
        .context("failed to prepare ax-std std clippy config")?;
    Ok(envs.into_iter().collect())
}

fn feature_clippy_env(
    package: &Package,
    feature: &str,
    base_env: Vec<(String, String)>,
    axconfig_override: Option<&ClippyAxconfigOverride>,
    metadata: &Metadata,
) -> anyhow::Result<Vec<(String, String)>> {
    if package.name == AXSTD_STD_PACKAGE && feature == AXSTD_STD_DEFAULT_FEATURE {
        return axstd_std_clippy_env(metadata);
    }

    Ok(with_axconfig_env_override(base_env, axconfig_override))
}

fn expand_clippy_checks(
    packages: &[SelectedClippyPackage],
    metadata: &Metadata,
) -> anyhow::Result<Vec<ClippyCheck>> {
    let mut checks = Vec::new();
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();

    for selected in packages {
        let package = &selected.package;
        let mut features: BTreeSet<_> = package
            .features
            .keys()
            .filter(|feature| feature.as_str() != DEFAULT_FEATURE)
            .cloned()
            .collect();
        if package.name == AXSTD_STD_PACKAGE {
            features.insert(AXSTD_STD_DEFAULT_FEATURE.to_string());
        }
        let targets = docs_rs_targets(package);
        let target_iter = if targets.is_empty() {
            vec![None]
        } else {
            targets.into_iter().map(Some).collect()
        };
        let env = clippy_env(package);
        let axconfig_overrides = feature_axconfig_overrides(package);

        for target in target_iter {
            checks.push(ClippyCheck {
                package: package.name.to_string(),
                kind: ClippyCheckKind::Base,
                deps_mode: selected.deps_mode.clone(),
                target: target.clone(),
                env: env.clone(),
                axconfig_override: None,
            });

            for feature in &features {
                if !feature_supported_on_clippy_target(package, feature, target.as_deref()) {
                    continue;
                }
                let axconfig_override = axconfig_overrides.get(feature).and_then(|overrides| {
                    clippy_axconfig_override(
                        package,
                        target.as_deref(),
                        feature,
                        overrides,
                        &workspace_root,
                    )
                });
                let feature_env = feature_clippy_env(
                    package,
                    feature,
                    env.clone(),
                    axconfig_override.as_ref(),
                    metadata,
                )
                .with_context(|| {
                    format!(
                        "failed to prepare clippy env for `{}` feature `{feature}`",
                        package.name
                    )
                })?;
                checks.push(ClippyCheck {
                    package: package.name.to_string(),
                    kind: ClippyCheckKind::Feature(feature.clone()),
                    deps_mode: selected.deps_mode.clone(),
                    target: target.clone(),
                    env: feature_env,
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

fn planned_clippy_report(checks: &[ClippyCheck]) -> ClippyRunReport {
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

    ClippyRunReport {
        total_checks: checks.len(),
        passed_checks: 0,
        packages,
    }
}

fn print_clippy_check_plan(workspace_root: &Path, index: usize, total: usize, check: &ClippyCheck) {
    let args = check.cargo_args();
    println!("[{}/{}] {}", index + 1, total, check.label());
    if check.env.is_empty() {
        println!(
            "          cd {} && cargo {}",
            workspace_root.display(),
            args.join(" ")
        );
    } else {
        println!(
            "          cd {} && {} cargo {}",
            workspace_root.display(),
            check.env_prefix(),
            args.join(" ")
        );
    }
}

fn run_clippy_checks<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    checks: &[ClippyCheck],
) -> anyhow::Result<ClippyRunReport> {
    let mut report = planned_clippy_report(checks);
    let package_indexes = report
        .packages
        .iter()
        .enumerate()
        .map(|(index, package)| (package.package.clone(), index))
        .collect::<HashMap<_, _>>();

    for (index, check) in checks.iter().enumerate() {
        print_clippy_check_plan(workspace_root, index, checks.len(), check);

        let package_index = package_indexes[check.package.as_str()];
        let package_report = &mut report.packages[package_index];
        package_report.total_checks += 1;

        let success = runner.run_clippy(workspace_root, check)?;

        if success {
            report.passed_checks += 1;
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

    Ok(report)
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

    fn metadata_with_resolve(packages: Vec<Package>, deps: &[(&str, &[&str])]) -> Metadata {
        let members = packages
            .iter()
            .map(|package| package.id.repr.as_str())
            .collect::<Vec<_>>();
        let ids = packages
            .iter()
            .map(|package| (package.name.as_str(), package.id.repr.as_str()))
            .collect::<HashMap<_, _>>();
        let nodes = deps
            .iter()
            .map(|(name, deps)| {
                serde_json::json!({
                    "id": ids[name],
                    "dependencies": deps.iter().map(|dep| ids[dep]).collect::<Vec<_>>(),
                    "deps": deps.iter().map(|dep| {
                        serde_json::json!({
                            "name": dep,
                            "pkg": ids[dep],
                            "dep_kinds": [{ "kind": null, "target": null }]
                        })
                    }).collect::<Vec<_>>(),
                    "features": []
                })
            })
            .collect::<Vec<_>>();
        let value = serde_json::json!({
            "packages": packages,
            "workspace_members": members,
            "workspace_default_members": members,
            "resolve": { "nodes": nodes, "root": null },
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
        let selected = packages
            .iter()
            .cloned()
            .map(|package| SelectedClippyPackage {
                package,
                deps_mode: ClippyDepsMode::NoDeps,
            })
            .collect::<Vec<_>>();
        expand_clippy_checks(&selected, &metadata_for_packages(packages))
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
                .map(|pkg| pkg.package.name.as_str())
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
                .map(|pkg| pkg.package.name.as_str())
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
                    deps_mode: ClippyDepsMode::NoDeps,
                    target: None,
                    env: Vec::new(),
                    axconfig_override: None,
                },
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Feature("feat-a".into()),
                    deps_mode: ClippyDepsMode::NoDeps,
                    target: None,
                    env: Vec::new(),
                    axconfig_override: None,
                },
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Feature("feat-b".into()),
                    deps_mode: ClippyDepsMode::NoDeps,
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
    fn incremental_selection_keeps_runnable_top_levels_when_some_are_skipped() {
        let packages = vec![
            pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
            pkg("axvm", "axvm 0.1.0 (path+file:///tmp/axvm)", &[], None),
            pkg(
                "axvisor",
                "axvisor 0.1.0 (path+file:///tmp/axvisor)",
                &[],
                None,
            ),
            pkg("app", "app 0.1.0 (path+file:///tmp/app)", &[], None),
        ];
        let metadata = metadata_with_resolve(
            packages.clone(),
            &[
                ("alpha", &[]),
                ("axvm", &["alpha"]),
                ("axvisor", &["axvm"]),
                ("app", &["axvm"]),
            ],
        );

        let selected = incremental_clippy_selections(
            vec!["alpha".into()],
            vec![
                "alpha".into(),
                "axvm".into(),
                "axvisor".into(),
                "app".into(),
            ],
            &metadata,
            &packages,
        );

        assert_eq!(
            selected,
            vec![
                ("alpha".into(), ClippyDepsMode::NoDeps),
                ("app".into(), ClippyDepsMode::WithDeps),
            ]
        );
    }

    #[test]
    fn incremental_selection_falls_back_when_all_top_levels_are_skipped() {
        let packages = vec![
            pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
            pkg("axvm", "axvm 0.1.0 (path+file:///tmp/axvm)", &[], None),
            pkg(
                "axvisor",
                "axvisor 0.1.0 (path+file:///tmp/axvisor)",
                &[],
                None,
            ),
        ];
        let metadata = metadata_with_resolve(
            packages.clone(),
            &[("alpha", &[]), ("axvm", &["alpha"]), ("axvisor", &["axvm"])],
        );

        let selected = incremental_clippy_selections(
            vec!["alpha".into()],
            vec!["alpha".into(), "axvm".into(), "axvisor".into()],
            &metadata,
            &packages,
        );

        assert_eq!(
            selected,
            vec![
                ("alpha".into(), ClippyDepsMode::NoDeps),
                ("axvm".into(), ClippyDepsMode::WithDeps),
            ]
        );
    }

    #[test]
    fn incremental_selection_recomputes_frontier_around_skipped_top_level() {
        // `shared` is depended on by both a runnable top-level (`app`) and the
        // skipped top-level (`axvisor`). `axvm` sits only under `axvisor`, so
        // merely dropping skipped top-levels would leave `axvm` unlinted. The
        // frontier must be recomputed over `affected \ skipped` so `axvm` is
        // re-promoted to a runnable with-deps root.
        let packages = vec![
            pkg(
                "shared",
                "shared 0.1.0 (path+file:///tmp/shared)",
                &[],
                None,
            ),
            pkg("app", "app 0.1.0 (path+file:///tmp/app)", &[], None),
            pkg("axvm", "axvm 0.1.0 (path+file:///tmp/axvm)", &[], None),
            pkg(
                "axvisor",
                "axvisor 0.1.0 (path+file:///tmp/axvisor)",
                &[],
                None,
            ),
        ];
        let metadata = metadata_with_resolve(
            packages.clone(),
            &[
                ("shared", &[]),
                ("app", &["shared"]),
                ("axvm", &["shared"]),
                ("axvisor", &["axvm"]),
            ],
        );

        let selected = incremental_clippy_selections(
            vec!["shared".into()],
            vec![
                "app".into(),
                "axvm".into(),
                "axvisor".into(),
                "shared".into(),
            ],
            &metadata,
            &packages,
        );

        assert_eq!(
            selected,
            vec![
                ("shared".into(), ClippyDepsMode::NoDeps),
                ("app".into(), ClippyDepsMode::WithDeps),
                ("axvm".into(), ClippyDepsMode::WithDeps),
            ]
        );
    }

    #[test]
    fn incremental_selection_uses_natural_frontier_when_nothing_is_skipped() {
        let packages = vec![
            pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
            pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
            pkg("gamma", "gamma 0.1.0 (path+file:///tmp/gamma)", &[], None),
        ];
        let metadata = metadata_with_resolve(
            packages.clone(),
            &[("alpha", &[]), ("beta", &["alpha"]), ("gamma", &["beta"])],
        );

        let selected = incremental_clippy_selections(
            vec!["alpha".into()],
            vec!["alpha".into(), "beta".into(), "gamma".into()],
            &metadata,
            &packages,
        );

        assert_eq!(
            selected,
            vec![
                ("alpha".into(), ClippyDepsMode::NoDeps),
                ("gamma".into(), ClippyDepsMode::WithDeps),
            ]
        );
    }

    #[test]
    fn incremental_selection_keeps_changed_unsupported_crate_for_shared_skip_handling() {
        // Editing an unsupported crate's own source (e.g. `axvisor`) keeps it in
        // the `changed` selection instead of dropping it here; the shared
        // `skip_unsupported_packages` pass then removes it and prints the skip
        // message, matching `--all`/default behaviour.
        let packages = vec![pkg(
            "axvisor",
            "axvisor 0.1.0 (path+file:///tmp/axvisor)",
            &[],
            None,
        )];
        let metadata = metadata_with_resolve(packages.clone(), &[("axvisor", &[])]);

        let selected = incremental_clippy_selections(
            vec!["axvisor".into()],
            vec!["axvisor".into()],
            &metadata,
            &packages,
        );

        assert_eq!(selected, vec![("axvisor".into(), ClippyDepsMode::NoDeps)]);
    }

    #[test]
    fn with_deps_check_omits_no_deps_flag() {
        let check = ClippyCheck {
            package: "alpha".into(),
            kind: ClippyCheckKind::Base,
            deps_mode: ClippyDepsMode::WithDeps,
            target: None,
            env: Vec::new(),
            axconfig_override: None,
        };

        assert_eq!(
            check.cargo_args(),
            vec!["clippy", "-p", "alpha", "--", "-D", "warnings"]
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
                deps_mode: ClippyDepsMode::NoDeps,
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
            vec!["clippy", "--no-deps", "-p", "alpha", "--", "-D", "warnings"]
        );
        assert_eq!(
            checks[1].cargo_args(),
            vec![
                "clippy",
                "--no-deps",
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
                "--no-deps",
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
                "--no-deps",
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
                "--no-deps",
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
    fn ax_hal_platform_features_are_filtered_by_target_arch() {
        let checks = expand(&[pkg(
            "ax-hal",
            "ax-hal 0.1.0 (path+file:///tmp/ax-hal)",
            &[
                ("irq", &[]),
                ("loongarch64-qemu-virt", &[]),
                ("x86-pc", &[]),
            ],
            Some(&["loongarch64-unknown-none", "x86_64-unknown-none"]),
        )]);

        let has_feature_on_target = |feature: &str, target: &str| {
            checks.iter().any(|check| {
                matches!(&check.kind, ClippyCheckKind::Feature(check_feature) if check_feature == feature)
                    && check.target.as_deref() == Some(target)
            })
        };

        assert!(has_feature_on_target(
            "irq",
            "loongarch64-unknown-none-softfloat"
        ));
        assert!(has_feature_on_target("irq", "x86_64-unknown-none"));
        assert!(has_feature_on_target(
            "loongarch64-qemu-virt",
            "loongarch64-unknown-none-softfloat"
        ));
        assert!(!has_feature_on_target(
            "loongarch64-qemu-virt",
            "x86_64-unknown-none"
        ));
        assert!(has_feature_on_target("x86-pc", "x86_64-unknown-none"));
        assert!(!has_feature_on_target(
            "x86-pc",
            "loongarch64-unknown-none-softfloat"
        ));
    }

    #[test]
    fn ax_hal_target_only_features_are_skipped_for_host_clippy() {
        let checks = expand(&[pkg(
            "ax-hal",
            "ax-hal 0.1.0 (path+file:///tmp/ax-hal)",
            &[("irq", &[]), ("plat-dyn", &[]), ("x86-pc", &[])],
            None,
        )]);

        assert!(checks.iter().any(|check| {
            matches!(&check.kind, ClippyCheckKind::Feature(feature) if feature == "irq")
        }));
        assert!(!checks.iter().any(|check| {
            matches!(&check.kind, ClippyCheckKind::Feature(feature) if feature == "plat-dyn")
        }));
        assert!(!checks.iter().any(|check| {
            matches!(&check.kind, ClippyCheckKind::Feature(feature) if feature == "x86-pc")
        }));
    }

    #[test]
    fn ax_hal_platform_feature_forwards_are_filtered_by_target_arch() {
        let checks = expand(&[pkg(
            "platform-forwarder",
            "platform-forwarder 0.1.0 (path+file:///tmp/platform-forwarder)",
            &[
                ("irq", &["ax-hal/irq"]),
                ("loongarch64-qemu-virt", &["ax-hal/loongarch64-qemu-virt"]),
                ("x86-pc", &["ax-hal/x86-pc"]),
            ],
            Some(&["loongarch64-unknown-none", "x86_64-unknown-none"]),
        )]);

        let has_feature_on_target = |feature: &str, target: &str| {
            checks.iter().any(|check| {
                matches!(&check.kind, ClippyCheckKind::Feature(check_feature) if check_feature == feature)
                    && check.target.as_deref() == Some(target)
            })
        };

        assert!(has_feature_on_target(
            "irq",
            "loongarch64-unknown-none-softfloat"
        ));
        assert!(has_feature_on_target("irq", "x86_64-unknown-none"));
        assert!(has_feature_on_target(
            "loongarch64-qemu-virt",
            "loongarch64-unknown-none-softfloat"
        ));
        assert!(!has_feature_on_target(
            "loongarch64-qemu-virt",
            "x86_64-unknown-none"
        ));
        assert!(has_feature_on_target("x86-pc", "x86_64-unknown-none"));
        assert!(!has_feature_on_target(
            "x86-pc",
            "loongarch64-unknown-none-softfloat"
        ));
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
                "--no-deps",
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
            vec!["clippy", "--no-deps", "-p", "alpha", "--", "-D", "warnings"]
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

        let packages = packages
            .into_iter()
            .map(|package| SelectedClippyPackage {
                package,
                deps_mode: ClippyDepsMode::NoDeps,
            })
            .collect();
        let filtered = skip_unsupported_packages(packages);

        assert_eq!(
            filtered
                .iter()
                .map(|package| package.package.name.as_str())
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
    fn axstd_default_config_is_passed_as_clippy_env() {
        let metadata = crate::build::workspace_metadata().unwrap();
        let package = metadata
            .packages
            .iter()
            .find(|package| package.name == AXSTD_STD_PACKAGE)
            .cloned()
            .expect("ax-std package should be in workspace metadata");

        let checks = expand_clippy_checks(
            &[SelectedClippyPackage {
                package,
                deps_mode: ClippyDepsMode::NoDeps,
            }],
            &metadata,
        )
        .unwrap();

        assert!(
            checks[0].env.is_empty(),
            "base ax-std clippy check should not use std-build env: {:?}",
            checks[0].env
        );

        let std_check = checks
            .iter()
            .find(|check| {
                matches!(
                    &check.kind,
                    ClippyCheckKind::Feature(feature) if feature == AXSTD_STD_DEFAULT_FEATURE
                )
            })
            .expect("ax-std default clippy check should exist");

        assert!(
            std_check.env.iter().any(|(key, value)| {
                key == AX_CONFIG_PATH_ENV
                    && value
                        .ends_with("tmp/axbuild/axconfig/ax-std/x86_64-unknown-none/.axconfig.toml")
            }),
            "expected {AX_CONFIG_PATH_ENV} in {:?}",
            std_check.env
        );
        assert!(
            !std_check.env.iter().any(|(key, _)| key == "RUSTFLAGS"),
            "std ax-std clippy check should not inject custom RUSTFLAGS: {:?}",
            std_check.env
        );
        assert!(
            std_check
                .cargo_args()
                .windows(2)
                .any(|window| window == ["--features", AXSTD_STD_CLIPPY_FEATURES]),
            "expected expanded ax-std std features in {:?}",
            std_check.cargo_args()
        );
    }

    #[test]
    fn package_failures_abort_remaining_checks() {
        let root = PathBuf::from("/tmp/workspace");
        let checks = vec![
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Base,
                deps_mode: ClippyDepsMode::NoDeps,
                target: None,
                env: Vec::new(),
                axconfig_override: None,
            },
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Feature("feat-a".into()),
                deps_mode: ClippyDepsMode::NoDeps,
                target: None,
                env: Vec::new(),
                axconfig_override: None,
            },
            ClippyCheck {
                package: "beta".into(),
                kind: ClippyCheckKind::Base,
                deps_mode: ClippyDepsMode::NoDeps,
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

    #[test]
    fn elapsed_format_uses_largest_needed_units() {
        assert_eq!(format_elapsed(Duration::from_millis(250)), "250ms");
        assert_eq!(format_elapsed(Duration::from_secs(42)), "42s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m 5s");
        assert_eq!(format_elapsed(Duration::from_secs(3661)), "1h 1m 1s");
    }
}
