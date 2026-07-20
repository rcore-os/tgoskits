use super::{
    assets::arceos_test_group_dir,
    discovery::{discover_qemu_cases_in_dir, discover_qemu_cases_in_dir_allow_empty},
    runner::run_prepared_qemu_groups,
    rust_qemu::prepare_rust_qemu_cases,
    types::GenericQemuRunOptions,
};
use crate::arceos::ArceOS;

pub(super) async fn test_generic_qemu(
    arceos: &mut ArceOS,
    arch: &str,
    target: &str,
    group: &str,
    options: GenericQemuRunOptions<'_>,
) -> anyhow::Result<()> {
    let dir = arceos_test_group_dir(arceos.app.workspace_root(), group);
    let cases = if options.allow_empty && options.selected_case.is_none() {
        discover_qemu_cases_in_dir_allow_empty(&dir, arch, target, options.selected_case, group)?
    } else {
        discover_qemu_cases_in_dir(&dir, arch, target, options.selected_case, group)?
    };
    if cases.is_empty() {
        println!(
            "skipping arceos {group} qemu tests for arch: {arch} (target: {target}, no cases)"
        );
        return Ok(());
    }
    let group_label = format!("arceos {group}");
    println!(
        "running {group_label} qemu tests for arch: {arch} (target: {target}, cases: {})",
        cases.len()
    );
    let prepared = prepare_rust_qemu_cases(arceos, target, cases).await?;

    run_prepared_qemu_groups(
        arceos,
        group,
        &group_label,
        &prepared,
        options.symbolize_after,
        options.keep_qemu_log,
    )
    .await
}
