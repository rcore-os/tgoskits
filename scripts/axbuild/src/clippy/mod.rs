use std::time::Instant;

use anyhow::{Context, bail};
use chrono::Local;

mod check;
mod env;
mod expand;
mod report;
mod runner;
mod selection;
mod targets;
mod timing;

#[cfg(test)]
mod tests;

use expand::expand_clippy_checks;
use report::print_report_summary;
use runner::{ProcessCargoRunner, run_clippy_checks};
use selection::{
    clippy_metadata_needs_deps, resolve_requested_packages, skip_unsupported_packages,
    validate_clippy_args, workspace_packages,
};
use timing::print_clippy_timing;

pub(super) const DEFAULT_FEATURE: &str = "default";
pub(super) const AX_CONFIG_PATH_ENV: &str = "AX_CONFIG_PATH";
pub(super) const AXCONFIG_FILE: &str = "axconfig.toml";
pub(super) const AXSTD_STD_PACKAGE: &str = "ax-std";
pub(super) const AXSTD_STD_DEFAULT_FEATURE: &str = "default";
pub(super) const AXSTD_STD_CLIPPY_FEATURES: &str = "std-compat,plat-dyn,fs,multitask,irq,net";
pub(super) const AXSTD_STD_CLIPPY_TARGET: &str = "x86_64-unknown-none";
pub(super) const DOCS_RS_METADATA: &str = "docs.rs";
pub(super) const DOCS_METADATA: &str = "docs";
pub(super) const RS_METADATA: &str = "rs";
pub(super) const TARGETS_METADATA: &str = "targets";
pub(super) const AXBUILD_METADATA: &str = "axbuild";
pub(super) const CLIPPY_FEATURE_AXCONFIG_OVERRIDES_METADATA: &str =
    "clippy-feature-axconfig-overrides";
pub(super) const AX_HAL_PACKAGE: &str = "ax-hal";

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
