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
    crate::rootfs::inject::extract_rootfs(case_rootfs, &layout.staging_root)?;
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
            ("phase", "find-qemu-user".to_string()),
        ],
    );
    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    timing_stage.finish();
    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "prepare-cross-env".to_string()),
        ],
    );
    let build_env = prepare_host_cross_build_env(arch, layout, &qemu_runner)?;
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
        .exec()
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
    let result = build.exec().context("failed to build case C project");
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
    let result = install.exec().context("failed to install case C project");
    timing_stage.finish();
    result?;

    let timing_stage = timing::TimingStage::new(
        "qemu-asset-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "sync-runtime-deps".to_string()),
        ],
    );
    crate::rootfs::runtime::sync_runtime_dependencies(&layout.staging_root, &layout.overlay_dir)?;
    timing_stage.finish();
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
