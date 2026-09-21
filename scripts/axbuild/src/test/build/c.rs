use super::*;

pub(crate) fn case_c_source_dir(case: &TestQemuCase) -> PathBuf {
    case.case_dir.join(CASE_C_DIR_NAME)
}

pub(super) fn grouped_c_root_project_path(case: &TestQemuCase) -> PathBuf {
    case.case_dir.join(CASE_CMAKE_FILE_NAME)
}

pub(super) fn grouped_c_subcase_source_dir(subcase: &TestQemuSubcase) -> PathBuf {
    let legacy_c_dir = subcase.case_dir.join(CASE_C_DIR_NAME);
    if legacy_c_dir.is_dir() {
        legacy_c_dir
    } else {
        subcase.case_dir.clone()
    }
}

/// Returns the optional prebuild script path for a C-based QEMU case.
pub(crate) fn case_prebuild_script_path(case: &TestQemuCase) -> PathBuf {
    case_c_source_dir(case).join(CASE_PREBUILD_SCRIPT_NAME)
}

/// Absolute guest path of the musl cross `ld` in a toolchain-bearing rootfs.
fn cross_tool_ld(arch: &str) -> anyhow::Result<String> {
    let spec = super::toolchain::cross_compile_spec(arch)?;
    Ok(format!("/{}/ld", spec.guest_tool_dir))
}

/// Resolves the build-sysroot image to extract for a C case, if it differs
/// from the case rootfs.
///
/// C cases are cross-compiled against an Alpine musl sysroot. When the case
/// rootfs itself carries that toolchain (the default Alpine managed image and
/// any custom image prepared the same way) it stays the staging sysroot. A
/// runtime-only rootfs -- e.g. a Debian glibc userland -- must not be
/// compiled against, so the managed toolchain image is returned for
/// extraction instead; built artifacts are still injected into the case
/// rootfs. `None` means "extract the case rootfs".
pub(crate) fn c_toolchain_rootfs(
    workspace_root: &Path,
    target_dir: &Path,
    arch: &str,
    case_rootfs: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    if crate::rootfs::inject::ext4_image_contains_file(case_rootfs, &cross_tool_ld(arch)?)? {
        return Ok(None);
    }

    let Ok(toolchain_rootfs) =
        crate::image::storage::default_rootfs_path(workspace_root, target_dir, arch)
    else {
        return Ok(None);
    };
    if toolchain_rootfs == case_rootfs {
        return Ok(None);
    }
    Ok(Some(toolchain_rootfs))
}

/// Selects the extracted staging sysroot for a C case (see
/// [`c_toolchain_rootfs`]).
fn select_staging_sysroot(
    arch: &str,
    case_rootfs: &Path,
    toolchain_rootfs: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let case_has_toolchain =
        crate::rootfs::inject::ext4_image_contains_file(case_rootfs, &cross_tool_ld(arch)?)?;
    select_staging_sysroot_decided(case_rootfs, toolchain_rootfs, case_has_toolchain)
}

/// Pure decision core of [`select_staging_sysroot`], separated for tests.
pub(super) fn select_staging_sysroot_decided(
    case_rootfs: &Path,
    toolchain_rootfs: Option<&Path>,
    case_has_toolchain: bool,
) -> anyhow::Result<PathBuf> {
    if case_has_toolchain {
        return Ok(case_rootfs.to_path_buf());
    }
    match toolchain_rootfs {
        Some(toolchain_rootfs) => Ok(toolchain_rootfs.to_path_buf()),
        // No toolchain candidate is available: keep the legacy behavior so
        // the cross-build fails with the established missing-tool error
        // instead of a new one.
        None => Ok(case_rootfs.to_path_buf()),
    }
}

pub(super) fn grouped_c_subcase_prebuild_script_path(subcase: &TestQemuSubcase) -> PathBuf {
    grouped_c_subcase_source_dir(subcase).join(CASE_PREBUILD_SCRIPT_NAME)
}

/// Returns the optional prebuild script path for a Rust-based QEMU case.
pub(crate) fn case_rust_prebuild_script_path(case: &TestQemuCase) -> PathBuf {
    case_rust_source_dir(case).join(CASE_PREBUILD_SCRIPT_NAME)
}

/// Prepares rootfs-backed assets for a C-based QEMU test case.
pub(crate) fn prepare_c_case_assets_sync(
    arch: &str,
    case: &TestQemuCase,
    case_rootfs: &Path,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
) -> anyhow::Result<()> {
    prepare_c_case_overlay_sync(arch, case, case_rootfs, layout, config)?;
    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "inject-overlay".to_string()),
        ],
    );
    let result = crate::rootfs::inject::inject_overlay(case_rootfs, &layout.overlay_dir);
    timing_stage.finish();
    result
}

/// Builds a C case into its overlay without injecting that overlay into a rootfs.
///
/// Board tests use the resulting overlay as their session upload root.
pub(crate) fn prepare_c_case_overlay_sync(
    arch: &str,
    case: &TestQemuCase,
    case_rootfs: &Path,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
) -> anyhow::Result<()> {
    let source_dir = case_c_source_dir(case);
    let cmake_lists = source_dir.join(CASE_CMAKE_FILE_NAME);
    ensure!(
        cmake_lists.is_file(),
        "missing case CMake project entry `{}`",
        cmake_lists.display()
    );

    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "reset-layout".to_string()),
        ],
    );
    case_assets::reset_dir(&layout.staging_root)?;
    case_assets::reset_dir(&layout.build_dir)?;
    case_assets::reset_dir(&layout.overlay_dir)?;
    case_assets::reset_dir(&layout.command_wrapper_dir)?;
    case_assets::reset_dir(&layout.cross_bin_dir)?;
    fs::create_dir_all(&layout.apk_cache_dir)
        .with_context(|| format!("failed to create {}", layout.apk_cache_dir.display()))?;
    timing_stage.finish();

    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "extract-rootfs".to_string()),
        ],
    );
    let toolchain_rootfs =
        c_toolchain_rootfs(&layout.workspace_root, &layout.target_dir, arch, case_rootfs)?;
    let staging_sysroot = select_staging_sysroot(arch, case_rootfs, toolchain_rootfs.as_deref())?;
    crate::rootfs::inject::extract_rootfs(&staging_sysroot, &layout.staging_root)?;
    timing_stage.finish();
    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "prepare-staging-root".to_string()),
        ],
    );
    (config.prepare_staging_root)(&layout.staging_root)?;
    timing_stage.finish();
    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "write-musl-loader".to_string()),
        ],
    );
    write_musl_loader_search_path(arch, &layout.staging_root)?;
    timing_stage.finish();
    let prebuild_script = case_prebuild_script_path(case);
    if prebuild_script.is_file() {
        let timing_stage = timing::TimingStage::new(
            "qemu-asset-c",
            [
                ("case", case.display_name.clone()),
                ("phase", "prebuild".to_string()),
            ],
        );
        let extra_script_envs = prepare_guest_package_env(config, &layout.staging_root)?;
        let prebuild_env =
            prepare_guest_prebuild_env(arch, case, layout, extra_script_envs, config)?;
        let mut command = build_prebuild_command(case, &prebuild_script, layout, &prebuild_env)?;
        let result = command.exec().context("failed to run case prebuild.sh");
        timing_stage.finish();
        result?;
    }
    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "prepare-cross-env".to_string()),
        ],
    );
    let build_env = prepare_host_cross_build_env(arch, layout)?;
    timing_stage.finish();

    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "cmake-configure".to_string()),
        ],
    );
    let mut configure = build_cmake_configure_command(case, layout, &build_env, config);
    let result = configure
        .exec_quiet()
        .context("failed to configure case C project");
    timing_stage.finish();
    result?;

    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "cmake-build".to_string()),
        ],
    );
    let mut build = build_cmake_build_command(layout, &build_env);
    let result = build.exec_quiet().context("failed to build case C project");
    timing_stage.finish();
    result?;

    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "cmake-install".to_string()),
        ],
    );
    let mut install = build_cmake_install_command(layout, &build_env);
    let result = install
        .exec_quiet()
        .context("failed to install case C project");
    timing_stage.finish();
    result?;

    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "sync-runtime-deps".to_string()),
        ],
    );
    crate::rootfs::runtime::sync_runtime_dependencies(
        arch,
        &layout.staging_root,
        &layout.overlay_dir,
    )?;
    timing_stage.finish();
    Ok(())
}
