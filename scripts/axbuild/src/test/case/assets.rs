use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, bail};

use super::{
    cache::{
        copy_file_fast, is_valid_rootfs_cache_image, rootfs_cache_image_path,
        save_rootfs_cache_image,
    },
    case_sh_source_dir,
    layout::case_asset_layout,
    prepare_sh_case_assets_sync,
    types::{
        CaseAssetConfig, CaseAssetLayout, CasePipeline, PreparedCaseAssetParts, PreparedCaseAssets,
        TestQemuCase,
    },
};
use crate::test::{build as case_builder, timing};

/// Prepares any case-specific rootfs assets required by a QEMU test.
///
/// A per-case rootfs copy is created only when the case requires pre-boot
/// injection (C / shell / Python / grouped pipelines); plain cases boot
/// directly from the shared image. The rootfs patcher owns QEMU write
/// isolation for both paths.
pub(crate) async fn prepare_case_assets(
    target_dir: &Path,
    arch: &str,
    target: &str,
    case: &TestQemuCase,
    rootfs_path: PathBuf,
    config: CaseAssetConfig,
) -> anyhow::Result<PreparedCaseAssets> {
    let target_dir = target_dir.to_path_buf();
    let arch = arch.to_string();
    let target = target.to_string();
    let case = case.clone();
    let config = config.clone();
    // A C case on a runtime-only rootfs (e.g. Debian) is cross-compiled
    // against the managed Alpine toolchain image. Make sure that image is
    // downloaded up front so the synchronous asset build stays file-only.
    let workspace_root = crate::context::workspace_root_path()?;
    if resolve_case_pipeline(&case)? == CasePipeline::C
        && let Some(toolchain_rootfs) =
            case_builder::c_toolchain_rootfs(&workspace_root, &target_dir, &arch, &rootfs_path)?
        && !toolchain_rootfs.is_file()
    {
        crate::image::storage::ensure_rootfs_for_arch(&workspace_root, &target_dir, &arch).await?;
    }

    let parts = tokio::task::spawn_blocking(move || {
        prepare_case_assets_sync(&target_dir, &arch, &target, &case, &rootfs_path, &config)
    })
    .await
    .context("qemu test case asset task failed")??;

    Ok(parts.into())
}

/// Returns the Python source directory for a QEMU test case.
pub(crate) fn case_python_source_dir(case: &TestQemuCase) -> PathBuf {
    case_builder::case_python_source_dir(case)
}

/// Copies the shared rootfs image to a per-case working path.
///
/// The copy is always refreshed from the shared source so that leftover QEMU
/// guest writes from a previous run do not affect the current execution.
pub(crate) fn copy_shared_rootfs_for_case(
    shared_rootfs: &Path,
    layout: &CaseAssetLayout,
) -> anyhow::Result<()> {
    copy_file_fast(shared_rootfs, &layout.case_rootfs_copy)?;
    Ok(())
}

/// Owns one exclusively created case directory through preparation and execution.
#[derive(Debug)]
pub(super) struct CaseRunDirectory {
    path: PathBuf,
}

impl CaseRunDirectory {
    fn create(path: &Path) -> anyhow::Result<Self> {
        let parent = path
            .parent()
            .context("case run directory must have a parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        // Do not adopt an existing directory: only this successful creation
        // grants ownership, so cleanup cannot remove another run's assets.
        fs::create_dir(path).with_context(|| format!("failed to create {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for CaseRunDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "warning: failed to remove case run directory `{}`: {error}",
                self.path.display()
            );
        }
    }
}

/// Performs the synchronous part of QEMU case asset preparation.
///
/// Owns the temporary run directory as soon as it is created, so every failure
/// removes partial assets. Success transfers that ownership to the caller;
/// shared rootfs images and persistent caches remain outside this scope.
pub(crate) fn prepare_case_assets_sync(
    target_dir: &Path,
    arch: &str,
    target: &str,
    case: &TestQemuCase,
    shared_rootfs: &Path,
    config: &CaseAssetConfig,
) -> anyhow::Result<PreparedCaseAssetParts> {
    let timing_stage = timing::TimingStage::new(
        "qemu-asset",
        [
            ("case", case.display_name.clone()),
            ("phase", "resolve-pipeline".to_string()),
        ],
    );
    let pipeline = resolve_case_pipeline(case)?;
    timing_stage.finish();

    // Pipeline cases need per-case layout for injection work; plain cases boot
    // directly from the shared image without any layout.
    let needs_injection = pipeline != CasePipeline::Plain;
    let layout = if needs_injection {
        let timing_stage = timing::TimingStage::new(
            "qemu-asset",
            [
                ("case", case.display_name.clone()),
                ("phase", "create-layout".to_string()),
                ("pipeline", pipeline.as_str().to_string()),
            ],
        );
        let layout = case_asset_layout(target_dir, target, &case.display_name)?;
        timing_stage.finish();
        Some(layout)
    } else {
        None
    };

    let (case_rootfs, run_directory, cache_hit) = if needs_injection {
        let layout = layout.as_ref().expect("layout created above for injection");

        // Compute the cache key once and derive the cached rootfs image path.
        // The cached image is the post-injection rootfs -- ready for QEMU to
        // boot from directly. On a cache hit we skip inject_overlay entirely,
        // which is the dominant cost for Python/C pipeline cases.
        let timing_stage = timing::TimingStage::new(
            "qemu-asset",
            [
                ("case", case.display_name.clone()),
                ("phase", "rootfs-cache-key".to_string()),
                ("pipeline", pipeline.as_str().to_string()),
            ],
        );
        let rootfs_cache_img =
            rootfs_cache_image_path(layout, arch, target, pipeline, case, shared_rootfs, config)?;
        timing_stage.finish();

        let timing_stage = timing::TimingStage::new(
            "qemu-asset",
            [
                ("case", case.display_name.clone()),
                ("phase", "create-run-dir".to_string()),
                ("pipeline", pipeline.as_str().to_string()),
            ],
        );
        let run_directory = CaseRunDirectory::create(&layout.run_dir)?;
        timing_stage.finish();

        let cache_hit = if is_valid_rootfs_cache_image(&rootfs_cache_img) {
            // Cache HIT: copy/reflink the cached post-injection image. No need
            // to copy shared_rootfs, build an overlay, or run inject_overlay.
            let timing_stage = timing::TimingStage::new(
                "qemu-asset",
                [
                    ("case", case.display_name.clone()),
                    ("phase", "cache-hit-copy".to_string()),
                    ("pipeline", pipeline.as_str().to_string()),
                    ("cache", "hit".to_string()),
                ],
            );
            let result = copy_file_fast(&rootfs_cache_img, &layout.case_rootfs_copy);
            timing_stage.finish();
            result?;
            true
        } else {
            // Cache MISS: full pipeline build, then save result to cache.
            let timing_stage = timing::TimingStage::new(
                "qemu-asset",
                [
                    ("case", case.display_name.clone()),
                    ("phase", "copy-shared-rootfs".to_string()),
                    ("pipeline", pipeline.as_str().to_string()),
                    ("cache", "miss".to_string()),
                ],
            );
            let result = copy_shared_rootfs_for_case(shared_rootfs, layout);
            timing_stage.finish();
            result?;
            let copy = &layout.case_rootfs_copy;
            let timing_stage = timing::TimingStage::new(
                "qemu-asset",
                [
                    ("case", case.display_name.clone()),
                    ("phase", "pipeline-prepare".to_string()),
                    ("pipeline", pipeline.as_str().to_string()),
                    ("cache", "miss".to_string()),
                ],
            );
            let result = match pipeline {
                CasePipeline::Grouped => {
                    case_builder::prepare_grouped_case_assets_sync(arch, case, copy, layout, config)
                }
                CasePipeline::C => {
                    case_builder::prepare_c_case_assets_sync(arch, case, copy, layout, config)
                }
                CasePipeline::Sh => prepare_sh_case_assets_sync(case, copy, layout),
                CasePipeline::Python => {
                    case_builder::prepare_python_case_assets_sync(arch, case, copy, layout, config)
                }
                CasePipeline::Rust => {
                    case_builder::prepare_rust_case_assets_sync(arch, case, copy, layout, config)
                }
                CasePipeline::Plain => unreachable!("plain cases do not prepare injection assets"),
            };
            timing_stage.finish();
            result?;
            // Save the post-injection rootfs to cache so future runs can skip
            // the overlay build and inject_overlay steps entirely.
            let timing_stage = timing::TimingStage::new(
                "qemu-asset",
                [
                    ("case", case.display_name.clone()),
                    ("phase", "save-rootfs-cache".to_string()),
                    ("pipeline", pipeline.as_str().to_string()),
                    ("cache", "miss".to_string()),
                ],
            );
            let result = save_rootfs_cache_image(&layout.case_rootfs_copy, &rootfs_cache_img);
            timing_stage.finish();
            result?;
            false
        };

        (
            layout.case_rootfs_copy.clone(),
            Some(run_directory),
            cache_hit,
        )
    } else {
        // No injection needed -- boot directly from the shared image. The
        // rootfs patcher isolates guest writes, so no copy is required here.
        timing::print_timing_line(
            "qemu-asset",
            &[
                ("case", case.display_name.clone()),
                ("phase", "plain-rootfs".to_string()),
                ("pipeline", pipeline.as_str().to_string()),
            ],
            Duration::ZERO,
        );
        (shared_rootfs.to_path_buf(), None, false)
    };

    Ok(PreparedCaseAssetParts {
        rootfs_path: case_rootfs,
        run_directory,
        pipeline,
        cache_hit,
    })
}

pub(crate) fn resolve_case_pipeline(case: &TestQemuCase) -> anyhow::Result<CasePipeline> {
    let mut pipelines = Vec::new();
    if case.is_grouped() {
        pipelines.push(CasePipeline::Grouped);
    }
    if case_builder::case_c_source_dir(case).is_dir() {
        pipelines.push(CasePipeline::C);
    }
    if case_sh_source_dir(case).is_dir() {
        pipelines.push(CasePipeline::Sh);
    }
    if case_python_source_dir(case).is_dir() {
        pipelines.push(CasePipeline::Python);
    }
    if case_builder::case_rust_source_dir(case).is_dir() {
        pipelines.push(CasePipeline::Rust);
    }

    if pipelines.len() > 1 {
        bail!(
            "qemu case `{}` defines multiple asset pipelines: {}",
            case.name,
            pipelines
                .iter()
                .map(|pipeline| pipeline.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    Ok(pipelines.into_iter().next().unwrap_or(CasePipeline::Plain))
}
