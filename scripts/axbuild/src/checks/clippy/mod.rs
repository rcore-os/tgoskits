use std::time::Instant;

use anyhow::{Context, bail};
use chrono::Local;

mod check;
mod configurations;
mod env;
mod expand;
mod partition;
mod plan;
mod report;
mod runner;
mod scope;
mod selection;
mod targets;
mod timing;

#[cfg(test)]
mod tests;

use report::print_report_summary;
use runner::{ProcessCargoRunner, run_clippy_checks, run_parallel_clippy_checks};
use selection::{clippy_metadata_needs_deps, validate_clippy_args, workspace_packages};
use timing::print_clippy_timing;

pub(super) const DEFAULT_FEATURE: &str = "default";
pub(super) const HOST_TEST_FEATURE: &str = "host-test";
pub(super) const AXSTD_STD_PACKAGE: &str = "ax-std";
pub(super) const AXSTD_STD_DEFAULT_FEATURE: &str = "default";
pub(super) const AXSTD_STD_CLIPPY_FEATURES: &str = "std-compat,fs,net";
pub(super) const AXSTD_STD_CLIPPY_TARGET: &str = "x86_64-unknown-none";
pub(super) const AX_HAL_PACKAGE: &str = "ax-hal";

pub(crate) fn run_workspace_clippy_command(args: &crate::ClippyArgs) -> anyhow::Result<()> {
    validate_clippy_args(args)?;
    if args.print_selection {
        let manifest = crate::context::workspace_manifest_path()?;
        let metadata = crate::context::workspace_metadata_root_manifest_with_deps(&manifest)?;
        let root = metadata.workspace_root.as_std_path();
        let scope = scope::select(args, root, &metadata, &workspace_packages(&metadata))?;
        println!("{}", serde_json::to_string(&scope)?);
        return Ok(());
    }
    if args.emit_plan.is_some() && !args.all {
        bail!(
            "--emit-plan requires --all; saved plans always describe the complete supported matrix"
        );
    }
    if args.shards == 0 || args.shards > 256 {
        bail!("--shards must be between 1 and 256");
    }
    if args.execute_plan.is_some() || args.verify_results.is_some() {
        return plan::run_saved_plan(args);
    }
    let started_at = Local::now();
    let timer = Instant::now();
    println!(
        "clippy started at: {}",
        started_at.format("%Y-%m-%d %H:%M:%S %z")
    );
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = if args.emit_plan.is_some() || clippy_metadata_needs_deps(args) {
        crate::context::workspace_metadata_root_manifest_with_deps(&workspace_manifest)
    } else {
        crate::context::workspace_metadata_root_manifest(&workspace_manifest)
    }
    .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let target_dir = metadata.target_directory.clone().into_std_path_buf();
    let all_packages = workspace_packages(&metadata);
    let chosen = scope::select(args, &workspace_root, &metadata, &all_packages)?;
    let checks = chosen.checks;
    let packages = chosen.selected_packages;
    if checks.is_empty() {
        println!(
            "no supported Clippy checks selected ({})",
            chosen.reason_code
        );
        return Ok(());
    }

    println!(
        "running clippy for {} package(s) with {} check(s) from {}",
        packages.len(),
        checks.len(),
        workspace_root.display()
    );

    if let Some(output) = &args.emit_plan {
        let plan = plan::CheckPlan::create(&workspace_root, checks, args.shards)?;
        plan::write_json(output, &plan)?;
        println!("CLIPPY_PLAN: {}", serde_json::to_string(&plan.summary())?);
        return Ok(());
    }
    let mut runner = ProcessCargoRunner;
    let result = if args.jobs == 1 {
        run_clippy_checks(&mut runner, &workspace_root, &target_dir, &checks)
    } else {
        run_parallel_clippy_checks(&workspace_root, &target_dir, &checks, args.jobs)
    };
    let report = match result {
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
