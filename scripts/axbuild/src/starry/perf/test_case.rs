//! Test-suit asset reuse for scheduler performance experiments.

use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use ostool::run::qemu::QemuConfig;

use crate::{
    context::ResolvedStarryRequest,
    starry::{Starry, rootfs, test as starry_test},
    test::{case, qemu as qemu_test},
};

pub(super) struct PerfTestCase {
    selected: starry_test::StarryQemuCase,
}

impl PerfTestCase {
    pub(super) fn build_config_path(&self) -> &Path {
        &self.selected.build_config_path
    }

    pub(super) fn qemu_config_path(&self) -> &Path {
        &self.selected.case.qemu_config_path
    }
}

pub(super) fn resolve(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    selector: Option<&str>,
) -> anyhow::Result<Option<PerfTestCase>> {
    let Some(selector) = selector else {
        return Ok(None);
    };
    let mut cases = starry_test::discover_qemu_cases(workspace_root, arch, target, Some(selector))?;
    ensure!(
        cases.len() == 1,
        "Starry qperf test selector `{selector}` resolved to {} parent cases; expected exactly one",
        cases.len()
    );
    Ok(Some(PerfTestCase {
        selected: cases.remove(0),
    }))
}

pub(super) struct PreparedPerfTestCase {
    rootfs_copy_to_remove: Option<PathBuf>,
    run_dir_to_remove: Option<PathBuf>,
}

impl Drop for PreparedPerfTestCase {
    fn drop(&mut self) {
        case::remove_case_rootfs_copy(self.rootfs_copy_to_remove.as_deref());
        case::remove_case_run_dir(self.run_dir_to_remove.as_deref());
    }
}

pub(super) async fn prepare(
    starry: &Starry,
    request: &ResolvedStarryRequest,
    selected: Option<&PerfTestCase>,
    qemu: &mut QemuConfig,
) -> anyhow::Result<Option<PreparedPerfTestCase>> {
    let Some(selected) = selected else {
        return Ok(None);
    };

    let workspace_root = starry.app.workspace_root();
    Starry::rewrite_qemu_case_managed_rootfs_paths(workspace_root, qemu)?;
    let default_rootfs = crate::image::storage::default_rootfs_path(workspace_root, &request.arch)?;
    for rootfs_path in Starry::qemu_case_managed_rootfs_paths(workspace_root, qemu)? {
        crate::image::storage::ensure_optional_managed_rootfs(
            workspace_root,
            &request.arch,
            Some(&rootfs_path),
        )
        .await?;
    }
    let source_rootfs = Starry::qemu_case_rootfs_path(workspace_root, qemu, &default_rootfs)?;
    let assets = case::prepare_case_assets(
        workspace_root,
        &request.arch,
        &request.target,
        &selected.selected.case,
        source_rootfs,
        perf_case_asset_config(),
    )
    .await
    .with_context(|| {
        format!(
            "failed to prepare qperf assets for Starry test case `{}`",
            selected.selected.case.display_name
        )
    })?;

    rootfs::patch_rootfs(
        qemu,
        &assets.rootfs_path,
        rootfs::RootfsPatchMode::EnsureDiskBootNet,
    );
    qemu.args.extend(assets.extra_qemu_args);
    if qemu.uefi {
        qemu_test::apply_drive_snapshot_without_global_snapshot(qemu);
    }

    Ok(Some(PreparedPerfTestCase {
        rootfs_copy_to_remove: assets.rootfs_copy_to_remove,
        run_dir_to_remove: assets.run_dir_to_remove,
    }))
}

fn perf_case_asset_config() -> case::CaseAssetConfig {
    let mut config = starry_test::starry_case_asset_config();
    config.grouped_execution = case::GroupedCaseExecution::External;
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qperf_case_assets_do_not_autorun_the_grouped_case() {
        let config = perf_case_asset_config();

        assert_eq!(
            config.grouped_execution,
            case::GroupedCaseExecution::External
        );
    }
}
