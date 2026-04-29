//! Shared QEMU test case asset orchestration.
//!
//! Main responsibilities:
//! - Decide whether a test case needs extra build or injection work
//! - Prepare case-scoped work directories, overlays, and auxiliary QEMU assets
//! - Dispatch C, shell, and Python case flows before rootfs content injection

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, bail, ensure};
use ostool::run::qemu::QemuConfig;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::build as case_builder;

pub(crate) const GROUPED_CASE_RUNNER_PATH: &str = "/usr/bin/starry-run-case-tests";
pub(crate) const GROUPED_CASE_SUCCESS_REGEX: &str = r"(?m)^STARRY_GROUPED_TESTS_PASSED\s*$";
pub(crate) const GROUPED_CASE_FAIL_REGEX: &str = r"(?m)^STARRY_GROUPED_TEST_FAILED:";

const CASE_WORK_ROOT_NAME: &str = "qemu-cases";
const CASE_CACHE_DIR_NAME: &str = "cache";
const CASE_RUNS_DIR_NAME: &str = "runs";
/// Sub-directory under `cache_dir` that holds pre-injected rootfs images.
/// One image per cache key (`{sha256}.img`); present means ready to use.
const CASE_ROOTFS_CACHE_DIR_NAME: &str = "rootfs";
const CASE_STAGING_DIR_NAME: &str = "staging-root";
const CASE_BUILD_DIR_NAME: &str = "build";
const CASE_OVERLAY_DIR_NAME: &str = "overlay";
const CASE_COMMAND_WRAPPER_DIR_NAME: &str = "guest-bin";
const CASE_CROSS_BIN_DIR_NAME: &str = "cross-bin";
const CASE_CMAKE_TOOLCHAIN_FILE_NAME: &str = "cmake-toolchain.cmake";
const CASE_APK_CACHE_DIR_NAME: &str = "apk-cache";
const CASE_SH_DIR_NAME: &str = "sh";
const GROUPED_CASE_RUNNER_NAME: &str = "starry-run-case-tests";
const CASE_ROOTFS_COPY_NAME: &str = "case-rootfs.img";
/// QEMU global snapshot flag — all disk writes go to a temporary file and are
/// never committed back to the image, keeping the source image pristine.
const QEMU_SNAPSHOT_ARG: &str = "-snapshot";

static CASE_RUN_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestQemuCase {
    pub(crate) name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) qemu_config_path: PathBuf,
    pub(crate) test_commands: Vec<String>,
    pub(crate) subcases: Vec<TestQemuSubcase>,
}

impl TestQemuCase {
    pub(crate) fn is_grouped(&self) -> bool {
        !self.test_commands.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestQemuSubcaseKind {
    C,
    Rust,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestQemuSubcase {
    pub(crate) name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) kind: TestQemuSubcaseKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCaseAssets {
    /// Path of the rootfs image that QEMU should boot from.  For cases without
    /// pipeline injection this points directly to the shared source image; for
    /// cases that need injection it points to the per-case temporary copy.
    pub(crate) rootfs_path: PathBuf,
    pub(crate) extra_qemu_args: Vec<String>,
    /// Path of the temporary per-case rootfs copy to remove after the QEMU run,
    /// or `None` when the shared image was used directly (no injection needed).
    pub(crate) rootfs_copy_to_remove: Option<PathBuf>,
    pub(crate) run_dir_to_remove: Option<PathBuf>,
    pub(crate) pipeline: CasePipeline,
    pub(crate) cache_hit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCaseAssetParts {
    pub(crate) extra_qemu_args: Vec<String>,
    pub(crate) rootfs_path: PathBuf,
    pub(crate) rootfs_copy_to_remove: Option<PathBuf>,
    pub(crate) run_dir_to_remove: Option<PathBuf>,
    pub(crate) pipeline: CasePipeline,
    pub(crate) cache_hit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CasePipeline {
    Plain,
    Grouped,
    C,
    Sh,
    Python,
}

impl CasePipeline {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Grouped => "grouped",
            Self::C => "c",
            Self::Sh => "sh",
            Self::Python => "python",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaseAssetLayout {
    pub(crate) work_dir: PathBuf,
    pub(crate) run_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    /// Directory holding pre-injected rootfs cache images (`{hash}.img`).
    pub(crate) rootfs_cache_dir: PathBuf,
    pub(crate) staging_root: PathBuf,
    pub(crate) build_dir: PathBuf,
    pub(crate) overlay_dir: PathBuf,
    pub(crate) command_wrapper_dir: PathBuf,
    pub(crate) cross_bin_dir: PathBuf,
    pub(crate) cmake_toolchain_file: PathBuf,
    pub(crate) apk_cache_dir: PathBuf,
    /// Per-case copy of the shared rootfs image, used only when the case needs
    /// pipeline injection (C / shell / Python / grouped).  For plain cases no
    /// copy is created and QEMU's `-snapshot` flag keeps the shared image clean.
    pub(crate) case_rootfs_copy: PathBuf,
}

/// Resolves the workspace target directory used for a test build target.
pub(crate) fn resolve_target_dir(workspace_root: &Path, target: &str) -> anyhow::Result<PathBuf> {
    Ok(workspace_root.join("target").join(target))
}

/// Prepares any case-specific rootfs assets required by a QEMU test.
///
/// QEMU's `-snapshot` flag is always included in the returned
/// `extra_qemu_args` so that guest writes never persist to any image file.
/// A per-case rootfs copy is created only when the case requires pre-boot
/// injection (C / shell / Python / grouped pipelines); plain cases boot
/// directly from the shared image.
pub(crate) async fn prepare_case_assets(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case: &TestQemuCase,
    rootfs_path: PathBuf,
) -> anyhow::Result<PreparedCaseAssets> {
    let workspace_root = workspace_root.to_path_buf();
    let arch = arch.to_string();
    let target = target.to_string();
    let case = case.clone();
    let parts = tokio::task::spawn_blocking(move || {
        prepare_case_assets_sync(&workspace_root, &arch, &target, &case, &rootfs_path)
    })
    .await
    .context("qemu test case asset task failed")??;

    Ok(PreparedCaseAssets {
        rootfs_path: parts.rootfs_path,
        extra_qemu_args: parts.extra_qemu_args,
        rootfs_copy_to_remove: parts.rootfs_copy_to_remove,
        run_dir_to_remove: parts.run_dir_to_remove,
        pipeline: parts.pipeline,
        cache_hit: parts.cache_hit,
    })
}

/// Returns the shell-script source directory for a QEMU test case.
pub(crate) fn case_sh_source_dir(case: &TestQemuCase) -> PathBuf {
    case.case_dir.join(CASE_SH_DIR_NAME)
}

/// Returns the Python source directory for a QEMU test case.
pub(crate) fn case_python_source_dir(case: &TestQemuCase) -> PathBuf {
    case_builder::case_python_source_dir(case)
}

/// Builds the working directory layout used for a QEMU case asset run.
pub(crate) fn case_asset_layout(
    workspace_root: &Path,
    target: &str,
    case_name: &str,
) -> anyhow::Result<CaseAssetLayout> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    let work_dir = target_dir.join(CASE_WORK_ROOT_NAME).join(case_name);
    let run_dir = work_dir.join(CASE_RUNS_DIR_NAME).join(next_case_run_id());
    let cache_dir = work_dir.join(CASE_CACHE_DIR_NAME);

    Ok(CaseAssetLayout {
        staging_root: run_dir.join(CASE_STAGING_DIR_NAME),
        build_dir: run_dir.join(CASE_BUILD_DIR_NAME),
        overlay_dir: run_dir.join(CASE_OVERLAY_DIR_NAME),
        command_wrapper_dir: run_dir.join(CASE_COMMAND_WRAPPER_DIR_NAME),
        cross_bin_dir: run_dir.join(CASE_CROSS_BIN_DIR_NAME),
        cmake_toolchain_file: run_dir.join(CASE_CMAKE_TOOLCHAIN_FILE_NAME),
        apk_cache_dir: cache_dir.join(CASE_APK_CACHE_DIR_NAME),
        rootfs_cache_dir: cache_dir.join(CASE_ROOTFS_CACHE_DIR_NAME),
        case_rootfs_copy: run_dir.join(CASE_ROOTFS_COPY_NAME),
        cache_dir,
        run_dir,
        work_dir,
    })
}

/// Copies the shared rootfs image to a per-case working path.
///
/// The copy is always refreshed from the shared source so that leftover QEMU
/// guest writes from a previous run do not affect the current execution.
pub(crate) fn copy_shared_rootfs_for_case(
    shared_rootfs: &Path,
    layout: &CaseAssetLayout,
) -> anyhow::Result<()> {
    fs::copy(shared_rootfs, &layout.case_rootfs_copy).with_context(|| {
        format!(
            "failed to copy rootfs {} to {}",
            shared_rootfs.display(),
            layout.case_rootfs_copy.display()
        )
    })?;
    Ok(())
}

/// Removes the per-case rootfs copy after a test run completes, if one was
/// created.  Pass `None` when no copy was made (plain cases that boot from the
/// shared image directly).
///
/// The work directory (CMake build cache, APK cache, etc.) is intentionally
/// kept; only the large rootfs image is removed.  Failures are logged as
/// warnings so that a stale copy never masks an actual test failure.
pub(crate) fn remove_case_rootfs_copy(path: Option<&Path>) {
    let Some(path) = path else { return };
    if path.exists()
        && let Err(e) = fs::remove_file(path)
    {
        eprintln!(
            "warning: failed to remove case rootfs copy `{}`: {e}",
            path.display()
        );
    }
}

pub(crate) fn remove_case_run_dir(path: Option<&Path>) {
    let Some(path) = path else { return };
    if path.exists()
        && let Err(e) = fs::remove_dir_all(path)
    {
        eprintln!(
            "warning: failed to remove case run directory `{}`: {e}",
            path.display()
        );
    }
}

/// Performs the synchronous part of QEMU case asset preparation.
///
/// Returns `(extra_qemu_args, rootfs_path, rootfs_copy_to_remove)` where:
/// - `extra_qemu_args` always contains `-snapshot` so QEMU guest writes never
///   persist to any image file.
/// - `rootfs_path` is the image QEMU should boot from — the shared source
///   image for plain cases, or a fresh per-case copy for pipeline cases.
/// - `rootfs_copy_to_remove` is `Some(copy_path)` when a copy was created and
///   must be deleted after the run, `None` for plain cases.
pub(crate) fn prepare_case_assets_sync(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case: &TestQemuCase,
    shared_rootfs: &Path,
) -> anyhow::Result<PreparedCaseAssetParts> {
    let pipeline = resolve_case_pipeline(case)?;

    // Pipeline cases need per-case layout for injection work; plain cases boot
    // directly from the shared image without any layout.
    let needs_injection = pipeline != CasePipeline::Plain;
    let layout = if needs_injection {
        Some(case_asset_layout(workspace_root, target, &case.name)?)
    } else {
        None
    };

    let (case_rootfs, rootfs_copy_to_remove, run_dir_to_remove, cache_hit) = if needs_injection {
        let layout = layout.as_ref().expect("layout created above for injection");

        // Compute the cache key once and derive the cached rootfs image path.
        // The cached image is the post-injection rootfs — ready for QEMU to boot
        // from directly.  On a cache hit we skip inject_overlay entirely, which
        // is the dominant cost for Python/C pipeline cases.
        let rootfs_cache_img =
            rootfs_cache_image_path(layout, arch, target, pipeline, case, shared_rootfs)?;

        fs::create_dir_all(&layout.run_dir)
            .with_context(|| format!("failed to create {}", layout.run_dir.display()))?;

        let cache_hit = if is_valid_rootfs_cache_image(&rootfs_cache_img) {
            // Cache HIT: copy/reflink the cached post-injection image.
            // No need to copy shared_rootfs, build an overlay, or run inject_overlay.
            copy_file_fast(&rootfs_cache_img, &layout.case_rootfs_copy)?;
            true
        } else {
            // Cache MISS: full pipeline build, then save result to cache.
            copy_shared_rootfs_for_case(shared_rootfs, layout)?;
            let copy = &layout.case_rootfs_copy;
            match pipeline {
                CasePipeline::Grouped => {
                    case_builder::prepare_grouped_case_assets_sync(arch, case, copy, layout)?
                }
                CasePipeline::C => {
                    case_builder::prepare_c_case_assets_sync(arch, case, copy, layout)?
                }
                CasePipeline::Sh => prepare_sh_case_assets_sync(case, copy, layout)?,
                CasePipeline::Python => {
                    case_builder::prepare_python_case_assets_sync(arch, case, copy, layout)?
                }
                CasePipeline::Plain => unreachable!("plain cases do not prepare injection assets"),
            }
            // Save the post-injection rootfs to cache so future runs can skip
            // the overlay build and inject_overlay steps entirely.
            if let Some(parent) = rootfs_cache_img.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create rootfs cache dir {}", parent.display())
                })?;
            }
            copy_file_fast(&layout.case_rootfs_copy, &rootfs_cache_img)?;
            false
        };

        let copy = layout.case_rootfs_copy.clone();
        (
            copy.clone(),
            Some(copy),
            Some(layout.run_dir.clone()),
            cache_hit,
        )
    } else {
        // No injection needed — boot directly from the shared image.
        // QEMU's -snapshot (below) ensures the shared image is never modified
        // by guest writes, so no copy is required at all.
        (shared_rootfs.to_path_buf(), None, None, false)
    };

    // -snapshot is always passed: all QEMU guest writes go to a temporary file
    // that QEMU auto-deletes on exit, keeping both the shared image and any
    // injection copy pristine after the run.
    let extra_qemu_args = vec![QEMU_SNAPSHOT_ARG.to_string()];

    Ok(PreparedCaseAssetParts {
        extra_qemu_args,
        rootfs_path: case_rootfs,
        rootfs_copy_to_remove,
        run_dir_to_remove,
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

fn next_case_run_id() -> String {
    let sequence = CASE_RUN_ID.fetch_add(1, Ordering::Relaxed);
    format!("{}-{sequence}", std::process::id())
}

fn rootfs_cache_image_path(
    layout: &CaseAssetLayout,
    arch: &str,
    target: &str,
    pipeline: CasePipeline,
    case: &TestQemuCase,
    shared_rootfs: &Path,
) -> anyhow::Result<PathBuf> {
    let key = case_asset_cache_key(arch, target, pipeline, case, shared_rootfs)?;
    Ok(layout.rootfs_cache_dir.join(format!("{key}.img")))
}

/// Returns `true` when a rootfs cache image file exists and has a plausible
/// size.  Files smaller than 1 MiB are treated as corrupt/incomplete and will
/// trigger a cache-miss rebuild.
fn is_valid_rootfs_cache_image(path: &Path) -> bool {
    const MIN_SIZE: u64 = 1024 * 1024;
    path.is_file()
        && path
            .metadata()
            .map(|m| m.len() >= MIN_SIZE)
            .unwrap_or(false)
}

/// Copies `src` to `dst`, preferring a copy-on-write reflink when the
/// filesystem supports it so that ~1 GiB rootfs images are duplicated in
/// near-zero time on btrfs / XFS.
///
/// On Linux this delegates to `cp --reflink=auto` and falls back to a regular
/// `fs::copy` if that fails (e.g. on ext4 or when `cp` is too old).  On other
/// platforms only `fs::copy` is used.
fn copy_file_fast(src: &Path, dst: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // `cp --reflink=auto` tries FICLONE ioctl first; if the filesystem
        // does not support it the command falls back to a regular copy, so
        // this is always safe to try.
        let status = std::process::Command::new("cp")
            .arg("--reflink=auto")
            .arg(src)
            .arg(dst)
            .status();
        if let Ok(status) = status {
            if status.success() {
                return Ok(());
            }
            // cp reported an error — remove any partial destination file so
            // the fs::copy fallback below starts with a clean slate.
            let _ = fs::remove_file(dst);
        }
        // Fall through to regular copy on any failure (cp not available,
        // unsupported flag, or a non-CoW error we cannot distinguish).
    }
    fs::copy(src, dst)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))?;
    Ok(())
}

fn case_asset_cache_key(
    arch: &str,
    target: &str,
    pipeline: CasePipeline,
    case: &TestQemuCase,
    shared_rootfs: &Path,
) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    hash_token(&mut hasher, "v2");
    hash_token(&mut hasher, arch);
    hash_token(&mut hasher, target);
    hash_token(&mut hasher, case.name.as_str());
    hash_token(&mut hasher, pipeline.as_str());
    hash_token(
        &mut hasher,
        std::env::var(crate::starry::apk::STARRY_APK_REGION_VAR)
            .unwrap_or_default()
            .as_str(),
    );
    // Only the C pipeline uses the CMake toolchain template; include it in the
    // key only when relevant so that changes to the template don't invalidate
    // caches for unrelated pipelines.
    if pipeline == CasePipeline::C {
        hash_token(&mut hasher, include_str!("cmake-toolchain.cmake.in"));
    }

    hash_file_metadata(&mut hasher, shared_rootfs)?;
    hash_tree(&mut hasher, &case.case_dir)?;
    if !case.qemu_config_path.starts_with(&case.case_dir) && case.qemu_config_path.is_file() {
        hash_file(&mut hasher, &case.qemu_config_path)?;
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_tree(hasher: &mut Sha256, root: &Path) -> anyhow::Result<()> {
    let mut files = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to walk {}", root.display()))?;
    files.sort_by_key(|entry| entry.path().to_path_buf());

    for entry in files {
        let path = entry.path();
        if path == root || !entry.file_type().is_file() {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        hash_token(hasher, rel.to_string_lossy().as_ref());
        hash_file(hasher, path)?;
    }
    Ok(())
}

fn hash_file_metadata(hasher: &mut Sha256, path: &Path) -> anyhow::Result<()> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    // Hash only the file size, not mtime.  mtime is unreliable in Docker
    // volumes, NFS mounts, and many CI environments (files copied from a
    // container or extracted from a tarball often have synthetic timestamps).
    // The size alone is a sufficient signal that the rootfs has been replaced;
    // a rootfs update that produces a same-size image would normally also
    // change case source files (hashed via hash_tree) so false cache hits are
    // very unlikely in practice.
    hash_token(hasher, &metadata.len().to_string());
    Ok(())
}

fn hash_file(hasher: &mut Sha256, path: &Path) -> anyhow::Result<()> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut buf = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buf)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(())
}

fn hash_token(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_le_bytes());
    hasher.update(value.as_bytes());
}

pub(crate) fn apply_grouped_qemu_config(qemu: &mut QemuConfig, case: &TestQemuCase) {
    if !case.is_grouped() {
        return;
    }

    qemu.shell_init_cmd = Some(GROUPED_CASE_RUNNER_PATH.to_string());
    qemu.success_regex = vec![GROUPED_CASE_SUCCESS_REGEX.to_string()];
    if !qemu
        .fail_regex
        .iter()
        .any(|regex| regex == GROUPED_CASE_FAIL_REGEX)
    {
        qemu.fail_regex.push(GROUPED_CASE_FAIL_REGEX.to_string());
    }
}

pub(crate) fn write_grouped_case_runner_script(
    overlay_dir: &Path,
    test_commands: &[String],
) -> anyhow::Result<()> {
    ensure!(
        !test_commands.is_empty(),
        "grouped qemu case has no test commands"
    );

    let dest_dir = overlay_dir.join("usr/bin");
    fs::create_dir_all(&dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;
    let runner_path = dest_dir.join(GROUPED_CASE_RUNNER_NAME);

    let mut body = String::new();
    body.push_str("failed=0\n");
    for command in test_commands {
        let quoted = shell_single_quote(command);
        let begin = shell_single_quote(&format!("STARRY_GROUPED_TEST_BEGIN: {command}"));
        let passed = shell_single_quote(&format!("STARRY_GROUPED_TEST_PASSED: {command}"));
        let failed = shell_single_quote(&format!("STARRY_GROUPED_TEST_FAILED: {command}"));
        body.push_str(&format!(
            "printf '%s\\n' {begin}\nif sh -c {quoted}; then\n\tprintf '%s\\n' \
             {passed}\nelse\n\tstatus=$?\n\tprintf '%s status=%s\\n' {failed} \
             \"$status\"\n\tfailed=1\nfi\n"
        ));
    }
    body.push_str(
        "if [ \"$failed\" -eq 0 ]; then\n\techo STARRY_GROUPED_TESTS_PASSED\n\texit 0\nfi\necho \
         STARRY_GROUPED_TESTS_FAILED\nexit 1\n",
    );

    write_executable_script(&runner_path, &body)
}

/// Prepares overlay assets for a shell-based QEMU test case.
pub(crate) fn prepare_sh_case_assets_sync(
    case: &TestQemuCase,
    case_rootfs: &Path,
    layout: &CaseAssetLayout,
) -> anyhow::Result<()> {
    let sh_dir = case_sh_source_dir(case);
    ensure!(
        sh_dir.is_dir(),
        "sh directory not found at `{}`",
        sh_dir.display()
    );

    reset_dir(&layout.overlay_dir)?;

    let dest_dir = layout.overlay_dir.join("usr/bin");
    fs::create_dir_all(&dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;

    let mut entries = fs::read_dir(&sh_dir)
        .with_context(|| format!("failed to read {}", sh_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", sh_dir.display()))?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let dest = dest_dir.join(entry.file_name());
        fs::copy(&path, &dest)
            .with_context(|| format!("failed to copy {} to {}", path.display(), dest.display()))?;
        make_executable(&dest)?;
    }

    crate::rootfs::inject::inject_overlay(case_rootfs, &layout.overlay_dir)
}

/// Resets a directory to an empty existing state.
pub(crate) fn reset_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

fn write_executable_script(path: &Path, body: &str) -> anyhow::Result<()> {
    fs::write(path, format!("#!/bin/sh\nset -u\n{body}"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    make_executable(path)
}

fn make_executable(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn fake_case(root: &Path, name: &str) -> TestQemuCase {
        let case_dir = root.join("test-suit/starryos/normal").join(name);
        fs::create_dir_all(&case_dir).unwrap();
        TestQemuCase {
            name: name.to_string(),
            case_dir: case_dir.clone(),
            qemu_config_path: case_dir.join("qemu-aarch64.toml"),
            test_commands: Vec::new(),
            subcases: Vec::new(),
        }
    }

    #[test]
    fn resolve_target_dir_uses_workspace_target_directory() {
        let root = tempdir().unwrap();
        let dir = resolve_target_dir(root.path(), "x86_64-unknown-none").unwrap();

        assert_eq!(dir, root.path().join("target/x86_64-unknown-none"));
    }

    #[tokio::test]
    async fn prepare_case_assets_plain_case_uses_shared_rootfs_with_snapshot() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        let rootfs_dir = root.path().join("target/rootfs");
        fs::create_dir_all(&target_dir).unwrap();
        fs::create_dir_all(&rootfs_dir).unwrap();
        let shared_img = rootfs_dir.join("rootfs-x86_64-alpine.img");
        fs::write(&shared_img, b"rootfs").unwrap();
        let case = fake_case(root.path(), "smoke");

        let assets = prepare_case_assets(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            &case,
            shared_img.clone(),
        )
        .await
        .unwrap();

        // Plain case (no pipeline): rootfs_path must point to the shared image
        // directly — no per-case copy is created.
        assert_eq!(assets.rootfs_path, shared_img);
        assert!(assets.rootfs_copy_to_remove.is_none());
        // -snapshot must always be present so QEMU guest writes never dirty
        // the shared image.
        assert!(assets.extra_qemu_args.contains(&"-snapshot".to_string()));
        // The shared image must be unmodified.
        assert_eq!(fs::read(&shared_img).unwrap(), b"rootfs");
    }

    #[test]
    fn grouped_runner_script_runs_all_commands_and_reports_summary() {
        let root = tempdir().unwrap();
        let overlay = root.path().join("overlay");
        let commands = vec![
            "/usr/bin/alpha".to_string(),
            "/usr/bin/beta --flag".to_string(),
        ];

        write_grouped_case_runner_script(&overlay, &commands).unwrap();

        let runner = overlay.join("usr/bin/starry-run-case-tests");
        let content = fs::read_to_string(&runner).unwrap();
        assert!(content.contains("STARRY_GROUPED_TEST_BEGIN: /usr/bin/alpha"));
        assert!(content.contains("STARRY_GROUPED_TEST_FAILED: /usr/bin/beta --flag"));
        assert!(content.contains("STARRY_GROUPED_TESTS_PASSED"));
    }
}
