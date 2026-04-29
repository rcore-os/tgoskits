//! Shared QEMU test case asset orchestration.
//!
//! Main responsibilities:
//! - Decide whether a test case needs extra build or injection work
//! - Prepare case-scoped work directories, overlays, and auxiliary QEMU assets
//! - Dispatch C, shell, and Python case flows before rootfs content injection

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use ostool::run::qemu::QemuConfig;

use super::build as case_builder;

pub(crate) const GROUPED_CASE_RUNNER_PATH: &str = "/usr/bin/starry-run-case-tests";
pub(crate) const GROUPED_CASE_SUCCESS_REGEX: &str = r"(?m)^STARRY_GROUPED_TESTS_PASSED\s*$";
pub(crate) const GROUPED_CASE_FAIL_REGEX: &str = r"(?m)^STARRY_GROUPED_TEST_FAILED:";

const CASE_WORK_ROOT_NAME: &str = "qemu-cases";
const CASE_STAGING_DIR_NAME: &str = "staging-root";
const CASE_BUILD_DIR_NAME: &str = "build";
const CASE_OVERLAY_DIR_NAME: &str = "overlay";
const CASE_COMMAND_WRAPPER_DIR_NAME: &str = "guest-bin";
const CASE_CROSS_BIN_DIR_NAME: &str = "cross-bin";
const CASE_CMAKE_TOOLCHAIN_FILE_NAME: &str = "cmake-toolchain.cmake";
const CASE_APK_CACHE_DIR_NAME: &str = "apk-cache";
const CASE_SH_DIR_NAME: &str = "sh";
const GROUPED_CASE_RUNNER_NAME: &str = "starry-run-case-tests";
const USB_STICK_IMAGE_NAME: &str = "usb-stick.raw";
const USB_STICK_IMAGE_SIZE: u64 = 16 * 1024 * 1024;
const CASE_ROOTFS_COPY_NAME: &str = "case-rootfs.img";
/// QEMU global snapshot flag — all disk writes go to a temporary file and are
/// never committed back to the image, keeping the source image pristine.
const QEMU_SNAPSHOT_ARG: &str = "-snapshot";

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaseAssetLayout {
    pub(crate) work_dir: PathBuf,
    pub(crate) staging_root: PathBuf,
    pub(crate) build_dir: PathBuf,
    pub(crate) overlay_dir: PathBuf,
    pub(crate) command_wrapper_dir: PathBuf,
    pub(crate) cross_bin_dir: PathBuf,
    pub(crate) cmake_toolchain_file: PathBuf,
    pub(crate) apk_cache_dir: PathBuf,
    pub(crate) usb_stick_path: PathBuf,
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
    let (extra_qemu_args, rootfs_path, rootfs_copy_to_remove) =
        tokio::task::spawn_blocking(move || {
            prepare_case_assets_sync(&workspace_root, &arch, &target, &case, &rootfs_path)
        })
        .await
        .context("qemu test case asset task failed")??;

    Ok(PreparedCaseAssets {
        rootfs_path,
        extra_qemu_args,
        rootfs_copy_to_remove,
    })
}

/// Returns whether a QEMU test case uses the C pipeline.
pub(crate) fn case_uses_c_pipeline(case: &TestQemuCase) -> bool {
    case_builder::case_c_source_dir(case).is_dir()
}

/// Returns the shell-script source directory for a QEMU test case.
pub(crate) fn case_sh_source_dir(case: &TestQemuCase) -> PathBuf {
    case.case_dir.join(CASE_SH_DIR_NAME)
}

/// Returns whether a QEMU test case uses the shell pipeline.
pub(crate) fn case_uses_sh_pipeline(case: &TestQemuCase) -> bool {
    case_sh_source_dir(case).is_dir()
}

/// Returns the Python source directory for a QEMU test case.
pub(crate) fn case_python_source_dir(case: &TestQemuCase) -> PathBuf {
    case_builder::case_python_source_dir(case)
}

/// Returns whether a QEMU test case uses the Python pipeline.
pub(crate) fn case_uses_python_pipeline(case: &TestQemuCase) -> bool {
    case_python_source_dir(case).is_dir()
}

/// Returns whether a QEMU test case needs extra USB-backed assets.
pub(crate) fn case_uses_usb_qemu_assets(arch: &str, case: &TestQemuCase) -> bool {
    let _ = arch;
    let _ = case;
    false
}

/// Builds the working directory layout used for a QEMU case asset run.
pub(crate) fn case_asset_layout(
    workspace_root: &Path,
    target: &str,
    case_name: &str,
) -> anyhow::Result<CaseAssetLayout> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    let work_dir = target_dir.join(CASE_WORK_ROOT_NAME).join(case_name);

    Ok(CaseAssetLayout {
        staging_root: work_dir.join(CASE_STAGING_DIR_NAME),
        build_dir: work_dir.join(CASE_BUILD_DIR_NAME),
        overlay_dir: work_dir.join(CASE_OVERLAY_DIR_NAME),
        command_wrapper_dir: work_dir.join(CASE_COMMAND_WRAPPER_DIR_NAME),
        cross_bin_dir: work_dir.join(CASE_CROSS_BIN_DIR_NAME),
        cmake_toolchain_file: work_dir.join(CASE_CMAKE_TOOLCHAIN_FILE_NAME),
        apk_cache_dir: work_dir.join(CASE_APK_CACHE_DIR_NAME),
        usb_stick_path: work_dir.join(USB_STICK_IMAGE_NAME),
        case_rootfs_copy: work_dir.join(CASE_ROOTFS_COPY_NAME),
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
) -> anyhow::Result<(Vec<String>, PathBuf, Option<PathBuf>)> {
    let needs_injection = case.is_grouped()
        || case_uses_c_pipeline(case)
        || case_uses_sh_pipeline(case)
        || case_uses_python_pipeline(case);

    let (case_rootfs, rootfs_copy_to_remove) = if needs_injection {
        // A fresh per-case copy is required so inject_overlay writes land on
        // the copy rather than the shared image.  QEMU's -snapshot (below)
        // then prevents even QEMU guest writes from reaching the copy.
        let layout = case_asset_layout(workspace_root, target, &case.name)?;
        fs::create_dir_all(&layout.work_dir)
            .with_context(|| format!("failed to create {}", layout.work_dir.display()))?;
        copy_shared_rootfs_for_case(shared_rootfs, &layout)?;
        let copy = layout.case_rootfs_copy.clone();

        if case.is_grouped() {
            case_builder::prepare_grouped_case_assets_sync(arch, case, &copy, &layout)?;
        } else if case_uses_c_pipeline(case) {
            case_builder::prepare_c_case_assets_sync(arch, case, &copy, &layout)?;
        } else if case_uses_sh_pipeline(case) {
            prepare_sh_case_assets_sync(case, &copy, &layout)?;
        } else {
            case_builder::prepare_python_case_assets_sync(arch, case, &copy, &layout)?;
        }
        (copy.clone(), Some(copy))
    } else {
        // No injection needed — boot directly from the shared image.
        // QEMU's -snapshot (below) ensures the shared image is never modified
        // by guest writes, so no copy is required at all.
        (shared_rootfs.to_path_buf(), None)
    };

    // -snapshot is always passed: all QEMU guest writes go to a temporary file
    // that QEMU auto-deletes on exit, keeping both the shared image and any
    // injection copy pristine after the run.
    let mut extra_qemu_args = vec![QEMU_SNAPSHOT_ARG.to_string()];
    if case_uses_usb_qemu_assets(arch, case) {
        let layout = case_asset_layout(workspace_root, target, &case.name)?;
        create_usb_backing_image(&layout.usb_stick_path)?;
        extra_qemu_args.extend(usb_qemu_args(&layout.usb_stick_path));
    }

    Ok((extra_qemu_args, case_rootfs, rootfs_copy_to_remove))
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

/// Returns the extra QEMU arguments used for the synthetic USB backing image.
pub(crate) fn usb_qemu_args(usb_stick_path: &Path) -> Vec<String> {
    vec![
        "-device".to_string(),
        "qemu-xhci,id=xhci".to_string(),
        "-drive".to_string(),
        format!(
            "if=none,format=raw,file={},id=usbstick0",
            usb_stick_path.display()
        ),
        "-device".to_string(),
        "usb-storage,drive=usbstick0,bus=xhci.0".to_string(),
    ]
}

/// Creates the empty backing image used for USB-related QEMU test assets.
pub(crate) fn create_usb_backing_image(path: &Path) -> anyhow::Result<()> {
    let file =
        fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    file.set_len(USB_STICK_IMAGE_SIZE)
        .with_context(|| format!("failed to size {}", path.display()))
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
    fn case_asset_layout_and_usb_qemu_args_use_stable_paths() {
        let root = tempdir().unwrap();
        let layout =
            case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb").unwrap();

        assert_eq!(
            layout.work_dir,
            root.path()
                .join("target/aarch64-unknown-none-softfloat/qemu-cases/usb")
        );
        assert_eq!(
            usb_qemu_args(&layout.usb_stick_path),
            vec![
                "-device".to_string(),
                "qemu-xhci,id=xhci".to_string(),
                "-drive".to_string(),
                format!(
                    "if=none,format=raw,file={},id=usbstick0",
                    layout.usb_stick_path.display()
                ),
                "-device".to_string(),
                "usb-storage,drive=usbstick0,bus=xhci.0".to_string(),
            ]
        );
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
