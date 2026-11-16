//! Rust suite selection and target-specific execution policy.
mod catalog;
pub(crate) use catalog::{SuiteCatalog, load_catalog};
mod plan;
mod profile;
mod selection;

pub(crate) use plan::print_plan;
pub(crate) use selection::{
    render_listing, rust_qemu_features_for_list, rust_qemu_features_for_run,
};

use super::{
    discovery::discover_rust_qemu_cases, qemu::prepare_qemu_cases, runner::run_prepared_qemu_groups,
};
use crate::arceos::ArceOS;

pub(crate) async fn test_rust_qemu(
    arceos: &mut ArceOS,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<()> {
    let catalog = load_catalog()?;
    let cases = discover_rust_qemu_cases(
        arceos,
        arch,
        target,
        selected_case,
        allow_missing_selected_case,
        &catalog,
    )?;
    if cases.is_empty() {
        println!(
            "skipping arceos rust qemu tests for arch: {arch} (target: {target}, no matching \
             feature)"
        );
        return Ok(());
    }
    println!(
        "running arceos rust qemu tests for arch: {} (target: {}, cases: {})",
        arch,
        target,
        cases.len()
    );

    let prepared = prepare_qemu_cases(
        arceos,
        target,
        cases,
        Some(profile::apply_rust_qemu_feature_overrides),
    )
    .await?;
    run_prepared_qemu_groups(arceos, "rust", "arceos rust", &prepared).await
}
