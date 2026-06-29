use std::path::Path;

use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use super::{
    assets::{remove_case_rootfs_copy, remove_case_run_dir},
    types::{PreparedCaseAssets, RunPreparedQemuCaseOptions},
};
use crate::{context::AppContext, test::timing};

pub(crate) async fn run_qemu_with_prepared_case_assets(
    app: &mut AppContext,
    cargo: &Cargo,
    qemu: QemuConfig,
    capture_backtrace: Option<crate::backtrace::BacktraceQemuCapture>,
    qemu_config_path: &Path,
    prepared_assets: PreparedCaseAssets,
    options: RunPreparedQemuCaseOptions,
) -> anyhow::Result<()> {
    println!(
        "  prepare assets: {:.2?} (pipeline={}, cache={})",
        options.prepare_elapsed,
        prepared_assets.pipeline.as_str(),
        if prepared_assets.cache_hit {
            "hit"
        } else {
            "miss"
        }
    );
    println!(
        "  qemu config: {} (timeout={})",
        qemu_config_path.display(),
        super::super::qemu::qemu_timeout_summary(&qemu)
    );
    println!("  rootfs: {}", prepared_assets.rootfs_path.display());

    let qemu_started = std::time::Instant::now();
    let result = app.run_qemu(cargo, qemu, capture_backtrace).await;
    let qemu_elapsed = qemu_started.elapsed();
    println!("  qemu run: {:.2?}", qemu_elapsed);
    if let Some(mut fields) = options.qemu_timing_fields {
        fields.push(("phase", "qemu-run".to_string()));
        timing::print_timing_line("qemu-case", &fields, qemu_elapsed);
    }

    remove_case_rootfs_copy(prepared_assets.rootfs_copy_to_remove.as_deref());
    remove_case_run_dir(prepared_assets.run_dir_to_remove.as_deref());
    result
}
