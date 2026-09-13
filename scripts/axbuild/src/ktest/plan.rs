#[cfg(test)]
use std::future::Future;
use std::{collections::BTreeSet, path::PathBuf};

use anyhow::{anyhow, bail};

use super::KtestTarget;

pub(super) const X86_64_TARGET: &str = "x86_64-unknown-none";
pub(super) const AARCH64_TARGET: &str = "aarch64-unknown-none-softfloat";
pub(super) const RISCV64_TARGET: &str = "riscv64gc-unknown-none-elf";
pub(super) const LOONGARCH64_TARGET: &str = "loongarch64-unknown-none-softfloat";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum KtestRuntime {
    Arceos,
    Starry,
    Axvisor,
    Board,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiscoveredKtestPackage {
    pub(super) name: String,
    pub(super) manifest_path: PathBuf,
    pub(super) uses_workspace_axtest: bool,
    pub(super) runtime: KtestRuntime,
    pub(super) targets: Vec<KtestTarget>,
    pub(super) docs_rs_targets: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct QemuPlanSelector {
    pub(super) workspace: bool,
    pub(super) packages: Vec<String>,
    pub(super) excludes: Vec<String>,
    pub(super) tests: Vec<String>,
    pub(super) arch: Option<String>,
    pub(super) target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct KtestExecutionUnit {
    pub(super) package: String,
    pub(super) test: String,
    pub(super) runtime: KtestRuntime,
    pub(super) arch: String,
    pub(super) target: String,
}

pub(super) fn build_qemu_plan(
    packages: &[DiscoveredKtestPackage],
    selector: &QemuPlanSelector,
) -> anyhow::Result<Vec<KtestExecutionUnit>> {
    validate_selector(packages, selector)?;
    let selected_names = selected_package_names(packages, selector)?;
    let explicitly_selected = !selector.packages.is_empty();
    let requested_target = requested_target(selector)?;
    let mut plan = Vec::new();

    for package in packages
        .iter()
        .filter(|package| selected_names.contains(&package.name))
    {
        if !package.uses_workspace_axtest {
            if explicitly_selected {
                bail!(
                    "package `{}` must declare workspace `axtest` directly in [dev-dependencies]",
                    package.name
                );
            }
            continue;
        }
        if package.runtime == KtestRuntime::Board {
            if explicitly_selected {
                bail!(
                    "package `{}` uses the board axtest runtime; run it with `cargo xtask ktest \
                     board`",
                    package.name
                );
            }
            continue;
        }

        let targets = selected_test_targets(package, &selector.tests)?;
        if targets.is_empty() {
            continue;
        }
        let supported_targets = supported_targets(package)?;
        let execution_targets = match requested_target.as_ref() {
            Some(requested) if supported_targets.contains(requested) => vec![requested.clone()],
            Some(requested) if explicitly_selected => {
                bail!(
                    "package `{}` does not support requested axtest target `{requested}`",
                    package.name
                )
            }
            Some(_) => continue,
            None => supported_targets,
        };

        for target in targets {
            for triple in &execution_targets {
                plan.push(KtestExecutionUnit {
                    package: package.name.clone(),
                    test: target.name.clone(),
                    runtime: package.runtime,
                    arch: arch_for_target(triple)?.to_string(),
                    target: triple.clone(),
                });
            }
        }
    }

    if !selector.tests.is_empty() && plan.is_empty() {
        bail!(
            "no selected package has harness=false test target(s): {}",
            selector.tests.join(", ")
        );
    }
    plan.sort();
    plan.dedup();
    Ok(plan)
}

fn validate_selector(
    packages: &[DiscoveredKtestPackage],
    selector: &QemuPlanSelector,
) -> anyhow::Result<()> {
    if selector.workspace && !selector.packages.is_empty() {
        bail!("--workspace and --package are mutually exclusive");
    }
    if !selector.packages.is_empty() && !selector.excludes.is_empty() {
        bail!("--exclude requires workspace selection and cannot be combined with --package");
    }
    reject_duplicates("--package", &selector.packages)?;
    reject_duplicates("--exclude", &selector.excludes)?;
    reject_duplicates("--test", &selector.tests)?;

    let names = packages
        .iter()
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();
    for requested in selector.packages.iter().chain(&selector.excludes) {
        if !names.contains(requested.as_str()) {
            bail!("unknown workspace package `{requested}`");
        }
    }
    Ok(())
}

fn reject_duplicates(flag: &str, values: &[String]) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            bail!("duplicate {flag} value `{value}`");
        }
    }
    Ok(())
}

fn selected_package_names(
    packages: &[DiscoveredKtestPackage],
    selector: &QemuPlanSelector,
) -> anyhow::Result<BTreeSet<String>> {
    let mut selected = if selector.packages.is_empty() {
        packages
            .iter()
            .map(|package| package.name.clone())
            .collect::<BTreeSet<_>>()
    } else {
        selector.packages.iter().cloned().collect()
    };
    for excluded in &selector.excludes {
        selected.remove(excluded);
    }
    Ok(selected)
}

fn selected_test_targets<'a>(
    package: &'a DiscoveredKtestPackage,
    requested: &[String],
) -> anyhow::Result<Vec<&'a KtestTarget>> {
    let mut targets = package
        .targets
        .iter()
        .filter(|target| super::is_harness_false_test(target))
        .collect::<Vec<_>>();
    if targets.is_empty() {
        bail!(
            "package `{}` declares workspace axtest but has no harness=false [[test]] target in {}",
            package.name,
            package.manifest_path.display()
        );
    }
    if !requested.is_empty() {
        targets.retain(|target| requested.contains(&target.name));
    }
    targets.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(targets)
}

fn supported_targets(package: &DiscoveredKtestPackage) -> anyhow::Result<Vec<String>> {
    let Some(docs_targets) = package.docs_rs_targets.as_ref() else {
        return Ok(vec![X86_64_TARGET.to_string()]);
    };
    let mut targets = docs_targets
        .iter()
        .filter(|target| arch_for_target(target).is_ok())
        .cloned()
        .collect::<Vec<_>>();
    targets.sort_by_key(|target| arch_for_target(target).unwrap_or_default());
    targets.dedup();
    if targets.is_empty() {
        bail!(
            "package `{}` has [package.metadata.docs.rs].targets but no supported bare-metal \
             target",
            package.name
        );
    }
    Ok(targets)
}

fn requested_target(selector: &QemuPlanSelector) -> anyhow::Result<Option<String>> {
    if let Some(target) = &selector.target {
        arch_for_target(target)?;
        return Ok(Some(target.clone()));
    }
    selector
        .arch
        .as_deref()
        .map(target_for_arch)
        .transpose()
        .map(|target| target.map(str::to_string))
}

pub(super) fn target_for_arch(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "x86_64" => Ok(X86_64_TARGET),
        "aarch64" => Ok(AARCH64_TARGET),
        "riscv64" => Ok(RISCV64_TARGET),
        "loongarch64" => Ok(LOONGARCH64_TARGET),
        _ => bail!("unsupported axtest architecture `{arch}`"),
    }
}

pub(super) fn arch_for_target(target: &str) -> anyhow::Result<&'static str> {
    match target {
        X86_64_TARGET => Ok("x86_64"),
        AARCH64_TARGET => Ok("aarch64"),
        RISCV64_TARGET => Ok("riscv64"),
        LOONGARCH64_TARGET => Ok("loongarch64"),
        _ => Err(anyhow!("unsupported bare-metal axtest target `{target}`")),
    }
}

#[derive(Debug)]
pub(super) struct PlanFailure {
    pub(super) unit: KtestExecutionUnit,
    pub(super) error: anyhow::Error,
}

#[cfg(test)]
pub(super) async fn run_plan_units<F, Fut>(
    units: Vec<KtestExecutionUnit>,
    no_fail_fast: bool,
    mut run: F,
) -> Vec<PlanFailure>
where
    F: FnMut(KtestExecutionUnit) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let mut failures = Vec::new();
    for unit in units {
        if let Err(error) = run(unit.clone()).await {
            failures.push(PlanFailure { unit, error });
            if !no_fail_fast {
                break;
            }
        }
    }
    failures
}
