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

        let report_session = if check
            .target
            .as_deref()
            .and_then(crate::context::arch_for_target)
            == Some("aarch64")
        {
            Some(crate::build::start_future_incompat_report_session(
                &workspace_root.join("target"),
            )?)
        } else {
            None
        };
        let cargo_result = runner.run_clippy(workspace_root, check);
        let success =
            crate::build::finish_future_incompat_report_status(report_session, cargo_result)?;

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
        let invocation = check.cargo_invocation();
        let mut env = invocation.env;
        let target_dir = workspace_root.join("target").display().to_string();
        if let Some((_, value)) = env.iter_mut().find(|(key, _)| key == "CARGO_TARGET_DIR") {
            *value = target_dir;
        } else {
            env.push(("CARGO_TARGET_DIR".to_string(), target_dir));
        }
        run_cargo_status_with_env(workspace_root, &invocation.args, &env)
    }
}
