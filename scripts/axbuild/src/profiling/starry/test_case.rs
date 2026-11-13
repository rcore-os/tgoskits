//! Test-suit asset reuse for scheduler performance experiments.

use std::path::Path;

use anyhow::{Context, ensure};
use ostool::run::qemu::QemuConfig;

use crate::{
    context::ResolvedStarryRequest,
    execution::case,
    starry::{Starry, rootfs, test as starry_test},
};

pub(crate) struct PerfTestCase {
    selected: starry_test::StarryQemuCase,
}

impl PerfTestCase {
    pub(super) fn build_input(
        &self,
    ) -> &crate::build::ConfigInput<crate::starry::config::StarryConfig> {
        &self.selected.build_input
    }

    pub(crate) fn qemu_config_path(&self) -> &Path {
        &self.selected.case.qemu_config_path
    }
}

pub(crate) fn resolve(
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

pub(crate) struct PreparedPerfTestCase {
    _assets: case::PreparedCaseAssets,
}

pub(crate) async fn prepare(
    starry: &Starry,
    request: &ResolvedStarryRequest,
    selected: Option<&PerfTestCase>,
    qemu: &mut QemuConfig,
) -> anyhow::Result<Option<PreparedPerfTestCase>> {
    let Some(selected) = selected else {
        return Ok(None);
    };

    let workspace_root = starry.app.workspace_root();
    let target_dir = starry.app.target_dir();
    Starry::rewrite_qemu_case_managed_rootfs_paths(workspace_root, target_dir, qemu)?;
    let default_rootfs =
        crate::image::storage::default_rootfs_path(workspace_root, target_dir, &request.arch)?;
    for rootfs_path in Starry::qemu_case_managed_rootfs_paths(workspace_root, target_dir, qemu)? {
        crate::image::storage::ensure_optional_managed_rootfs(
            workspace_root,
            target_dir,
            &request.arch,
            Some(rootfs_path.as_path()),
        )
        .await?;
    }
    let source_rootfs =
        Starry::qemu_case_rootfs_path(workspace_root, target_dir, qemu, &default_rootfs)?;
    let assets = case::prepare_case_assets(
        target_dir,
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
        rootfs::RootfsPatchOptions {
            mode: rootfs::RootfsPatchMode::EnsureDiskBootNet,
            write_policy: rootfs::RootfsWritePolicy::Discard,
        },
    )?;

    Ok(Some(PreparedPerfTestCase { _assets: assets }))
}

fn perf_case_asset_config() -> case::CaseAssetConfig {
    let mut config = starry_test::starry_case_asset_config();
    config.grouped_execution = case::GroupedCaseExecution::External;
    config
}
