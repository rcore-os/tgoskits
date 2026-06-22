use std::{collections::HashMap, path::Path};

use anyhow::bail;

use super::{
    check::ClippyCheck,
    report::{ClippyRunReport, planned_clippy_report, print_clippy_check_plan},
};
use crate::support::process::run_cargo_status_with_env;

pub(super) fn run_clippy_checks<R: CargoRunner>(
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

pub(super) trait CargoRunner {
    fn run_clippy(&mut self, workspace_root: &Path, check: &ClippyCheck) -> anyhow::Result<bool>;
}

pub(super) struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run_clippy(&mut self, workspace_root: &Path, check: &ClippyCheck) -> anyhow::Result<bool> {
        if let Some(axconfig_override) = &check.axconfig_override {
            axconfig_override.generate(workspace_root)?;
        }
        let args = check.cargo_args();
        run_cargo_status_with_env(workspace_root, &args, &check.env)
    }
}
