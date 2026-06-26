//! Shared C/Python QEMU test case build orchestration.
//!
//! Main responsibilities:
//! - Prepare guest prebuild and host cross-build environments for C cases
//! - Generate toolchain and wrapper scripts used during case builds
//! - Run prebuild scripts and CMake configure/build/install steps
//! - Populate case overlays that will later be injected into the rootfs image

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, bail, ensure};

use super::{
    case as case_assets,
    case::{CaseAssetConfig, TestQemuCase, TestQemuSubcase, TestQemuSubcaseKind},
    timing,
};
use crate::{context::CrossCompileSpec, support::process::ProcessExt};

const CASE_C_DIR_NAME: &str = "c";
const CASE_PREBUILD_SCRIPT_NAME: &str = "prebuild.sh";
const CASE_CMAKE_FILE_NAME: &str = "CMakeLists.txt";
const CROSS_BINUTILS: &[&str] = &[
    "ld", "as", "ar", "ranlib", "strip", "nm", "objcopy", "objdump", "readelf",
];

#[derive(Debug, Clone)]
pub(crate) struct HostCrossBuildEnv {
    cmake: PathBuf,
    pkg_config: PathBuf,
    make_program: PathBuf,
    cmake_toolchain_file: PathBuf,
    command_envs: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub(crate) struct GuestPrebuildEnv {
    qemu_runner: PathBuf,
    script_envs: Vec<(String, String)>,
}

/// Returns the C source directory for a QEMU test case.
pub(crate) fn case_c_source_dir(case: &TestQemuCase) -> PathBuf {
    case.case_dir.join(CASE_C_DIR_NAME)
}

fn grouped_c_root_project_path(case: &TestQemuCase) -> PathBuf {
    case.case_dir.join(CASE_CMAKE_FILE_NAME)
}

fn grouped_c_subcase_source_dir(subcase: &TestQemuSubcase) -> PathBuf {
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

fn grouped_c_subcase_prebuild_script_path(subcase: &TestQemuSubcase) -> PathBuf {
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

/// Prepares assets for a grouped QEMU case containing multiple guest tests.
pub(crate) fn prepare_grouped_case_assets_sync(
    arch: &str,
    case: &TestQemuCase,
    case_rootfs: &Path,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
) -> anyhow::Result<()> {
    ensure!(
        case.is_grouped(),
        "case `{}` is not a grouped qemu case",
        case.name
    );

    let rust_subcases = case
        .subcases
        .iter()
        .filter(|subcase| subcase.kind == TestQemuSubcaseKind::Rust)
        .map(|subcase| subcase.name.as_str())
        .collect::<Vec<_>>();
    ensure!(
        rust_subcases.is_empty(),
        "grouped Rust test subcases are not supported yet: {}",
        rust_subcases.join(", ")
    );

    let timing_stage = timing::TimingStage::new(
        "qemu-asset-grouped",
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
        "qemu-asset-grouped",
        [
            ("case", case.display_name.clone()),
            ("phase", "extract-rootfs".to_string()),
        ],
    );
    crate::rootfs::inject::extract_rootfs(case_rootfs, &layout.staging_root)?;
    timing_stage.finish();
    let timing_stage = timing::TimingStage::new(
        "qemu-asset-grouped",
        [
            ("case", case.display_name.clone()),
            ("phase", "prepare-staging-root".to_string()),
        ],
    );
    (config.prepare_staging_root)(&layout.staging_root)?;
    timing_stage.finish();
    let timing_stage = timing::TimingStage::new(
        "qemu-asset-grouped",
        [
            ("case", case.display_name.clone()),
            ("phase", "write-musl-loader".to_string()),
        ],
    );
    write_musl_loader_search_path(arch, &layout.staging_root)?;
    timing_stage.finish();

    let timing_stage = timing::TimingStage::new(
        "qemu-asset-grouped",
        [
            ("case", case.display_name.clone()),
            ("phase", "select-c-subcases".to_string()),
        ],
    );
    let all_c_subcases = case
        .subcases
        .iter()
        .filter(|subcase| subcase.kind == TestQemuSubcaseKind::C)
        .collect::<Vec<_>>();
    let c_subcases = selected_grouped_c_subcases(case, all_c_subcases.clone())?;
    timing_stage.finish();

    if !c_subcases.is_empty() {
        let timing_stage = timing::TimingStage::new(
            "qemu-asset-grouped",
            [
                ("case", case.display_name.clone()),
                ("phase", "prepare-c-subcases".to_string()),
                ("subcase_count", c_subcases.len().to_string()),
            ],
        );
        let result = prepare_grouped_c_subcases_sync(
            arch,
            case,
            &c_subcases,
            all_c_subcases.len(),
            layout,
            config,
        );
        timing_stage.finish();
        result?;
    }

    let timing_stage = timing::TimingStage::new(
        "qemu-asset-grouped",
        [
            ("case", case.display_name.clone()),
            ("phase", "write-grouped-runner".to_string()),
        ],
    );
    let runner_commands = selected_grouped_runner_commands(case, &c_subcases)?;
    case_assets::write_grouped_case_runner_script(
        &layout.overlay_dir,
        &runner_commands,
        &config.grouped_runner,
    )?;
    timing_stage.finish();
    let timing_stage = timing::TimingStage::new(
        "qemu-asset-grouped",
        [
            ("case", case.display_name.clone()),
            ("phase", "sync-runtime-deps".to_string()),
        ],
    );
    crate::rootfs::runtime::sync_runtime_dependencies(&layout.staging_root, &layout.overlay_dir)?;
    timing_stage.finish();
    let timing_stage = timing::TimingStage::new(
        "qemu-asset-grouped",
        [
            ("case", case.display_name.clone()),
            ("phase", "inject-overlay".to_string()),
        ],
    );
    let result = crate::rootfs::inject::inject_overlay(case_rootfs, &layout.overlay_dir);
    timing_stage.finish();
    result
}

fn selected_grouped_c_subcases<'a>(
    case: &TestQemuCase,
    subcases: Vec<&'a TestQemuSubcase>,
) -> anyhow::Result<Vec<&'a TestQemuSubcase>> {
    if let Some(filter) = case
        .grouped_subcase_filter
        .as_ref()
        .filter(|filter| !filter.is_empty())
    {
        let known_names = subcases
            .iter()
            .map(|subcase| subcase.name.as_str())
            .collect::<BTreeSet<_>>();
        let missing = filter
            .iter()
            .filter(|name| !known_names.contains(name.as_str()))
            .map(String::as_str)
            .collect::<Vec<_>>();
        ensure!(
            missing.is_empty(),
            "grouped qemu case `{}` references unknown C subcase(s): {}",
            case.qemu_config_path.display(),
            missing.join(", ")
        );

        return Ok(subcases
            .into_iter()
            .filter(|subcase| filter.contains(subcase.name.as_str()))
            .collect());
    }

    let Some(command_names) = direct_usr_bin_command_names(&case.test_commands) else {
        return Ok(subcases);
    };

    let mut known_names = BTreeSet::new();
    let mut selected = Vec::new();
    for subcase in subcases {
        let subcase_names = grouped_c_subcase_binary_names(subcase)?;
        known_names.extend(subcase_names.iter().cloned());
        if subcase_names
            .iter()
            .any(|name| command_names.contains(name.as_str()))
        {
            selected.push(subcase);
        }
    }

    let missing = command_names
        .difference(&known_names)
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        missing.is_empty(),
        "grouped qemu case `{}` references test command(s) without C subcases: {}",
        case.qemu_config_path.display(),
        missing.join(", ")
    );

    Ok(selected)
}

fn selected_grouped_runner_commands(
    case: &TestQemuCase,
    selected_c_subcases: &[&TestQemuSubcase],
) -> anyhow::Result<Vec<String>> {
    let Some(filter) = case
        .grouped_subcase_filter
        .as_ref()
        .filter(|filter| !filter.is_empty())
    else {
        return Ok(case.test_commands.clone());
    };

    let Some(command_names) = direct_usr_bin_command_names(&case.test_commands) else {
        return Ok(case.test_commands.clone());
    };

    let mut selected_names = BTreeSet::new();
    for subcase in selected_c_subcases {
        selected_names.extend(grouped_c_subcase_binary_names(subcase)?);
    }

    let selected_commands = case
        .test_commands
        .iter()
        .filter(|command| {
            command
                .split_ascii_whitespace()
                .next()
                .and_then(|token| token.strip_prefix("/usr/bin/"))
                .is_some_and(|name| selected_names.contains(name))
        })
        .cloned()
        .collect::<Vec<_>>();

    ensure!(
        !selected_commands.is_empty(),
        "grouped qemu case `{}` filter {} selected no runner commands from direct /usr/bin \
         commands: {}",
        case.qemu_config_path.display(),
        filter.iter().cloned().collect::<Vec<_>>().join(", "),
        command_names.into_iter().collect::<Vec<_>>().join(", ")
    );

    Ok(selected_commands)
}

fn direct_usr_bin_command_names(commands: &[String]) -> Option<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for command in commands {
        let token = command.split_ascii_whitespace().next()?;
        let name = token.strip_prefix("/usr/bin/")?;
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return None;
        }
        names.insert(name.to_string());
    }
    Some(names)
}

fn grouped_c_subcase_binary_names(subcase: &TestQemuSubcase) -> anyhow::Result<BTreeSet<String>> {
    let mut names = BTreeSet::from([subcase.name.clone()]);
    let cmake_lists = grouped_c_subcase_source_dir(subcase).join(CASE_CMAKE_FILE_NAME);
    if cmake_lists.is_file() {
        let content = fs::read_to_string(&cmake_lists)
            .with_context(|| format!("failed to read {}", cmake_lists.display()))?;
        names.extend(cmake_install_target_names(&content));
    }
    Ok(names)
}

fn cmake_install_target_names(content: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in content.lines() {
        let line = line.split_once('#').map_or(line, |(code, _)| code);
        if !line.contains("install") || !line.contains("TARGETS") {
            continue;
        }

        let mut collect_targets = false;
        for token in line.split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '(' | ')')) {
            if token.is_empty() {
                continue;
            }

            let keyword = token.to_ascii_uppercase();
            if collect_targets {
                if matches!(
                    keyword.as_str(),
                    "ARCHIVE"
                        | "BUNDLE"
                        | "COMPONENT"
                        | "CONFIGURATIONS"
                        | "DESTINATION"
                        | "EXCLUDE_FROM_ALL"
                        | "FRAMEWORK"
                        | "LIBRARY"
                        | "NAMELINK_COMPONENT"
                        | "OBJECTS"
                        | "OPTIONAL"
                        | "PERMISSIONS"
                        | "RENAME"
                        | "RUNTIME"
                ) {
                    break;
                }
                names.insert(token.to_string());
            } else if keyword == "TARGETS" {
                collect_targets = true;
            }
        }
    }
    names
}

fn prepare_grouped_c_subcases_sync(
    arch: &str,
    case: &TestQemuCase,
    subcases: &[&TestQemuSubcase],
    all_c_subcase_count: usize,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
) -> anyhow::Result<()> {
    let timing_stage = timing::TimingStage::new(
        "grouped-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "find-qemu-user".to_string()),
        ],
    );
    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    timing_stage.finish();

    let root_prebuild_script = case.case_dir.join(CASE_PREBUILD_SCRIPT_NAME);
    if root_prebuild_script.is_file() {
        let timing_stage = timing::TimingStage::new(
            "grouped-c",
            [
                ("case", case.display_name.clone()),
                ("phase", "prebuild".to_string()),
            ],
        );
        let extra_script_envs = prepare_guest_package_env(config, &layout.staging_root)?;
        let prebuild_env =
            prepare_guest_prebuild_env(arch, case, layout, extra_script_envs, config)?;
        let mut command = build_prebuild_command_with_work_dir(
            &root_prebuild_script,
            &case.case_dir,
            layout,
            &prebuild_env,
        )?;
        let result = command
            .exec()
            .context("failed to run grouped C root prebuild.sh");
        timing_stage.finish();
        result?;
    }

    if subcases
        .iter()
        .any(|subcase| grouped_c_subcase_prebuild_script_path(subcase).is_file())
    {
        let timing_stage = timing::TimingStage::new(
            "grouped-c",
            [
                ("case", case.display_name.clone()),
                ("phase", "prepare-guest-package-env".to_string()),
            ],
        );
        let extra_script_envs = prepare_guest_package_env(config, &layout.staging_root)?;
        timing_stage.finish();

        for subcase in subcases {
            let subcase_started = std::time::Instant::now();
            let subcase_case = subcase_as_case(case, subcase);
            let subcase_layout = subcase_layout(layout, subcase.name.as_str());
            let prebuild_script = grouped_c_subcase_prebuild_script_path(subcase);
            if prebuild_script.is_file() {
                let timing_stage = timing::TimingStage::new(
                    "grouped-c",
                    [
                        ("case", case.display_name.clone()),
                        ("subcase", subcase.name.clone()),
                        ("phase", "prebuild".to_string()),
                    ],
                );
                let prebuild_env = prepare_guest_prebuild_env(
                    arch,
                    &subcase_case,
                    &subcase_layout,
                    extra_script_envs.clone(),
                    config,
                )?;
                let mut command = build_prebuild_command(
                    &subcase_case,
                    &prebuild_script,
                    &subcase_layout,
                    &prebuild_env,
                )?;
                let result = command.exec().with_context(|| {
                    format!("failed to run {} prebuild.sh", subcase.name.as_str())
                });
                timing_stage.finish();
                result?;
                timing::print_timing_line(
                    "grouped-c",
                    &[
                        ("case", case.display_name.clone()),
                        ("subcase", subcase.name.clone()),
                        ("phase", "prebuild-total".to_string()),
                    ],
                    subcase_started.elapsed(),
                );
            }
        }
    }

    let timing_stage = timing::TimingStage::new(
        "grouped-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "prepare-cross-env".to_string()),
        ],
    );
    let build_env = prepare_host_cross_build_env(arch, layout, &qemu_runner)?;
    timing_stage.finish();

    if grouped_c_root_project_path(case).is_file() {
        return prepare_grouped_c_root_project_sync(
            case,
            subcases,
            all_c_subcase_count,
            layout,
            config,
            &build_env,
        );
    }

    let mut subcase_timings = Vec::with_capacity(subcases.len());
    let compile_started = std::time::Instant::now();
    for subcase in subcases {
        let subcase_started = std::time::Instant::now();
        let subcase_layout = subcase_layout(layout, subcase.name.as_str());
        let cmake_lists = grouped_c_subcase_source_dir(subcase).join(CASE_CMAKE_FILE_NAME);
        ensure!(
            cmake_lists.is_file(),
            "missing grouped case CMake project entry `{}`",
            cmake_lists.display()
        );

        let timing_stage = timing::TimingStage::new(
            "grouped-c",
            [
                ("case", case.display_name.clone()),
                ("subcase", subcase.name.clone()),
                ("phase", "configure".to_string()),
            ],
        );
        let mut configure = build_cmake_configure_command_with_source_dir(
            &grouped_c_subcase_source_dir(subcase),
            &subcase_layout,
            &build_env,
            config,
        );
        let result = configure.exec().with_context(|| {
            format!(
                "failed to configure grouped C subcase `{}`",
                subcase.name.as_str()
            )
        });
        timing_stage.finish();
        result?;

        let timing_stage = timing::TimingStage::new(
            "grouped-c",
            [
                ("case", case.display_name.clone()),
                ("subcase", subcase.name.clone()),
                ("phase", "build".to_string()),
            ],
        );
        let mut build = build_cmake_build_command(&subcase_layout, &build_env);
        let result = build.exec().with_context(|| {
            format!(
                "failed to build grouped C subcase `{}`",
                subcase.name.as_str()
            )
        });
        timing_stage.finish();
        result?;

        let timing_stage = timing::TimingStage::new(
            "grouped-c",
            [
                ("case", case.display_name.clone()),
                ("subcase", subcase.name.clone()),
                ("phase", "install".to_string()),
            ],
        );
        let mut install = build_cmake_install_command(&subcase_layout, &build_env);
        let result = install.exec().with_context(|| {
            format!(
                "failed to install grouped C subcase `{}`",
                subcase.name.as_str()
            )
        });
        timing_stage.finish();
        result?;
        subcase_timings.push((subcase.name.clone(), subcase_started.elapsed()));
    }
    timing::print_grouped_c_compile_total(
        &case.display_name,
        "per-subcase",
        compile_started.elapsed(),
    );
    print_slowest_grouped_c_subcases(case, subcase_timings);

    Ok(())
}

fn prepare_grouped_c_root_project_sync(
    case: &TestQemuCase,
    subcases: &[&TestQemuSubcase],
    all_c_subcase_count: usize,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
    build_env: &HostCrossBuildEnv,
) -> anyhow::Result<()> {
    let cmake_lists = grouped_c_root_project_path(case);
    ensure!(
        cmake_lists.is_file(),
        "missing grouped case root CMake project entry `{}`",
        cmake_lists.display()
    );

    let compile_started = std::time::Instant::now();

    let timing_stage = timing::TimingStage::new(
        "grouped-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "configure-all".to_string()),
        ],
    );
    let mut configure = build_grouped_c_root_project_configure_command(
        case,
        subcases,
        all_c_subcase_count,
        layout,
        build_env,
        config,
    );
    let result = configure
        .exec()
        .context("failed to configure grouped C root project");
    timing_stage.finish();
    result?;

    let timing_stage = timing::TimingStage::new(
        "grouped-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "build-all".to_string()),
            ("subcase_count", subcases.len().to_string()),
        ],
    );
    let mut build = build_cmake_build_command(layout, build_env);
    let result = build
        .exec()
        .context("failed to build grouped C root project");
    timing_stage.finish();
    result?;

    let timing_stage = timing::TimingStage::new(
        "grouped-c",
        [
            ("case", case.display_name.clone()),
            ("phase", "install-all".to_string()),
        ],
    );
    let mut install = build_cmake_install_command(layout, build_env);
    let result = install
        .exec()
        .context("failed to install grouped C root project");
    timing_stage.finish();
    result?;

    timing::print_grouped_c_compile_total(
        &case.display_name,
        "root-project",
        compile_started.elapsed(),
    );

    Ok(())
}

fn print_slowest_grouped_c_subcases(case: &TestQemuCase, mut timings: Vec<(String, Duration)>) {
    timings.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    for (subcase, elapsed) in timings.into_iter().take(20) {
        timing::print_timing_line(
            "grouped-c",
            &[
                ("case", case.display_name.clone()),
                ("subcase", subcase),
                ("phase", "subcase-total".to_string()),
            ],
            elapsed,
        );
    }
}

fn subcase_layout(
    layout: &case_assets::CaseAssetLayout,
    subcase_name: &str,
) -> case_assets::CaseAssetLayout {
    let mut layout = layout.clone();
    layout.build_dir = layout.build_dir.join(subcase_name);
    layout
}

fn subcase_as_case(case: &TestQemuCase, subcase: &TestQemuSubcase) -> TestQemuCase {
    TestQemuCase {
        name: format!("{}/{}", case.name, subcase.name.as_str()),
        display_name: format!("{}/{}", case.display_name, subcase.name.as_str()),
        case_dir: subcase.case_dir.clone(),
        qemu_config_path: case.qemu_config_path.clone(),
        test_commands: Vec::new(),
        host_symbolize_success_regex: Vec::new(),
        host_http_server: case.host_http_server.clone(),
        subcases: Vec::new(),
        grouped_subcase_filter: None,
    }
}

/// Returns the Python source directory for a QEMU test case.
pub(crate) fn case_python_source_dir(case: &TestQemuCase) -> PathBuf {
    case.case_dir.join("python")
}

/// Prepares overlay assets for a Python-based QEMU test case.
///
/// This pipeline reuses the same staging rootfs and prebuild infrastructure as
/// the C pipeline, but instead of running CMake it:
/// 1. Installs `python3` via `apk add` inside the staging rootfs
/// 2. Copies `.py` files from the test's `python/` directory into `/usr/bin/`
/// 3. Builds an overlay from the Python installation and test scripts
/// 4. Injects the overlay into the rootfs image
pub(crate) fn prepare_python_case_assets_sync(
    arch: &str,
    case: &TestQemuCase,
    case_rootfs: &Path,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
) -> anyhow::Result<()> {
    let python_dir = case_python_source_dir(case);
    ensure!(
        python_dir.is_dir(),
        "missing case Python source directory `{}`",
        python_dir.display()
    );

    case_assets::reset_dir(&layout.staging_root)?;
    case_assets::reset_dir(&layout.overlay_dir)?;
    case_assets::reset_dir(&layout.command_wrapper_dir)?;
    fs::create_dir_all(&layout.apk_cache_dir)
        .with_context(|| format!("failed to create {}", layout.apk_cache_dir.display()))?;

    // Extract rootfs into staging
    crate::rootfs::inject::extract_rootfs(case_rootfs, &layout.staging_root)?;
    (config.prepare_staging_root)(&layout.staging_root)?;
    write_musl_loader_search_path(arch, &layout.staging_root)?;

    // Prepare guest prebuild environment and install python3
    let extra_script_envs = prepare_guest_package_env(config, &layout.staging_root)?;

    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    write_guest_command_wrappers(layout, &qemu_runner)?;

    // Build the prebuild command: run "apk add python3" inside the staging rootfs
    let guest_busybox = layout.staging_root.join("bin/busybox");
    let guest_shell = layout.staging_root.join("bin/sh");
    let mut prebuild_cmd = Command::new(&qemu_runner);
    prebuild_cmd.arg("-L").arg(&layout.staging_root);
    if guest_busybox.is_file() {
        prebuild_cmd.arg(&guest_busybox).arg("sh");
    } else {
        ensure!(
            guest_shell.is_file(),
            "staging root is missing guest shell `{}`",
            guest_shell.display()
        );
        prebuild_cmd.arg(&guest_shell);
    }
    prebuild_cmd.arg("-eu").arg("-c").arg("apk add python3");

    // Apply environment (PATH with wrappers, guest dynamic linker paths)
    let host_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = Vec::new();
    path_entries.push(layout.command_wrapper_dir.clone());
    path_entries.extend(std::env::split_paths(&host_path));
    let path = std::env::join_paths(path_entries)
        .map_err(|e| anyhow::anyhow!("failed to build guest prebuild PATH: {e}"))?;
    prebuild_cmd.env("PATH", path);
    prebuild_cmd.env("QEMU_LD_PREFIX", &layout.staging_root);
    prebuild_cmd.env("LD_LIBRARY_PATH", guest_library_path(&layout.staging_root));
    for (key, value) in extra_script_envs {
        prebuild_cmd.env(key, value);
    }

    prebuild_cmd
        .exec()
        .context("failed to install python3 via apk in staging rootfs")?;

    // Build overlay: copy Python installation from staging into overlay
    let python_dirs_to_copy: &[&str] = &["usr/bin", "usr/lib", "lib"];

    for rel_dir in python_dirs_to_copy {
        let src = layout.staging_root.join(rel_dir);
        let dst = layout.overlay_dir.join(rel_dir);
        if src.is_dir() {
            copy_dir_recursive(&src, &dst, &layout.staging_root)
                .with_context(|| format!("failed to copy {} to overlay", src.display()))?;
        }
    }

    // Copy .py test files into overlay at /usr/bin/
    let dest_bin = layout.overlay_dir.join("usr/bin");
    fs::create_dir_all(&dest_bin)
        .with_context(|| format!("failed to create {}", dest_bin.display()))?;

    for entry in fs::read_dir(&python_dir)
        .with_context(|| format!("failed to read {}", python_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let dest = dest_bin.join(entry.file_name());
        fs::copy(&path, &dest)
            .with_context(|| format!("failed to copy {} to {}", path.display(), dest.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dest)
                .with_context(|| format!("failed to stat {}", dest.display()))?
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dest, perms)
                .with_context(|| format!("failed to chmod {}", dest.display()))?;
        }
    }

    crate::rootfs::inject::inject_overlay(case_rootfs, &layout.overlay_dir)
}

/// Writes a musl loader search-path file into the staging root so that the
/// guest dynamic linker (`ld-musl-{arch}.so.1`) finds libraries under `/usr/lib`
/// and `/lib` at runtime.
///
/// The file is written to `/etc/ld-musl-{arch}.path` only when the
/// corresponding loader binary is present under `lib/`; otherwise this is a
/// no-op (the rootfs may not ship musl at all, or may use a different libc).
///
/// This is called from `prepare_c_case_assets_sync`,
/// `prepare_python_case_assets_sync`, and `prepare_grouped_case_assets_sync`
/// because those pipelines extract a staging rootfs, optionally run a
/// `prebuild.sh`, and cross-build guest binaries that link against musl.
///
/// It is *not* called from `prepare_sh_case_assets_sync`: shell test cases do
/// not extract a staging rootfs, do not cross-build C guest binaries, and do
/// not have a `prebuild.sh` phase — they only copy scripts from the case's
/// `sh/` source directory into the overlay and inject them directly.
fn write_musl_loader_search_path(arch: &str, staging_root: &Path) -> anyhow::Result<()> {
    let loader_path = staging_root
        .join("lib")
        .join(format!("ld-musl-{arch}.so.1"));
    if !loader_path.is_file() {
        return Ok(());
    }
    let etc_dir = staging_root.join("etc");
    fs::create_dir_all(&etc_dir)
        .with_context(|| format!("failed to create {}", etc_dir.display()))?;

    let path_file = etc_dir.join(format!("ld-musl-{arch}.path"));
    fs::write(&path_file, "/usr/lib\n/lib\n")
        .with_context(|| format!("failed to write {}", path_file.display()))?;

    Ok(())
}

/// Recursively copies a directory tree, preserving file permissions.
///
/// `allowed_root` is the canonical boundary: any symlink that resolves outside
/// this directory is rejected to prevent host filesystem leaks.
fn copy_dir_recursive(src: &Path, dst: &Path, allowed_root: &Path) -> anyhow::Result<()> {
    let canonical_root = fs::canonicalize(allowed_root).with_context(|| {
        format!(
            "failed to canonicalize allowed root {}",
            allowed_root.display()
        )
    })?;
    copy_dir_recursive_inner(src, dst, &canonical_root)
}

/// Inner recursive implementation that operates on an already-canonical root.
/// This avoids re-canonicalizing `allowed_root` on every recursion level.
fn copy_dir_recursive_inner(src: &Path, dst: &Path, canonical_root: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", src_path.display()))?;
        if file_type.is_dir() {
            copy_dir_recursive_inner(&src_path, &dst_path, canonical_root)?;
        } else if file_type.is_symlink() {
            // For symlinks: read the link target to decide what to do.
            let link_target = fs::read_link(&src_path)
                .with_context(|| format!("failed to read symlink {}", src_path.display()))?;

            // Resolve the symlink target within the guest rootfs, not the host.
            // Absolute guest symlinks (e.g. /bin/busybox) must be rebased onto
            // canonical_root before canonicalization; otherwise fs::canonicalize
            // would follow them through the host's "/" and produce a path that
            // trivially escapes the staging root.
            let host_target = if link_target.is_absolute() {
                // Strip the leading "/" so Path::join doesn't discard canonical_root.
                let rel = link_target.strip_prefix("/").unwrap_or(&link_target);
                canonical_root.join(rel)
            } else {
                // Relative symlink: resolve from the directory that contains it.
                src_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(&link_target)
            };

            match fs::canonicalize(&host_target) {
                Ok(resolved) => {
                    // Symlink resolves — verify it stays within the staging root.
                    ensure!(
                        resolved.starts_with(canonical_root),
                        "symlink `{}` resolves to `{}` which escapes the staging root `{}`",
                        src_path.display(),
                        resolved.display(),
                        canonical_root.display()
                    );
                    if resolved.is_file() {
                        fs::copy(&resolved, &dst_path).with_context(|| {
                            format!(
                                "failed to copy {} to {}",
                                resolved.display(),
                                dst_path.display()
                            )
                        })?;
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let mode = fs::metadata(&resolved)
                                .with_context(|| format!("failed to stat {}", resolved.display()))?
                                .permissions()
                                .mode();
                            fs::set_permissions(&dst_path, fs::Permissions::from_mode(mode))
                                .with_context(|| {
                                    format!("failed to chmod {}", dst_path.display())
                                })?;
                        }
                    }
                }
                Err(_) if link_target.is_relative() => {
                    // Dangling relative symlink — the target likely exists in the
                    // base rootfs already (e.g. busybox applet links). Safe to skip
                    // since the overlay only adds files; it doesn't need to duplicate
                    // links whose targets are already present in the base image.
                    continue;
                }
                Err(_) => {
                    // Dangling absolute symlink (rebased under staging root but still
                    // unresolvable) — skip it; the base rootfs should provide the target.
                    continue;
                }
            }
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&src_path)
                    .with_context(|| format!("failed to stat {}", src_path.display()))?
                    .permissions()
                    .mode();
                fs::set_permissions(&dst_path, fs::Permissions::from_mode(mode))
                    .with_context(|| format!("failed to chmod {}", dst_path.display()))?;
            }
        }
    }
    Ok(())
}

fn prepare_guest_prebuild_env(
    arch: &str,
    case: &TestQemuCase,
    layout: &case_assets::CaseAssetLayout,
    extra_script_envs: Vec<(String, String)>,
    config: &CaseAssetConfig,
) -> anyhow::Result<GuestPrebuildEnv> {
    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    write_guest_command_wrappers(layout, &qemu_runner)?;

    let mut script_envs = case_script_envs(case, layout, config);
    script_envs.extend(extra_script_envs);

    Ok(GuestPrebuildEnv {
        qemu_runner,
        script_envs,
    })
}

fn prepare_guest_package_env(
    config: &CaseAssetConfig,
    staging_root: &Path,
) -> anyhow::Result<Vec<(String, String)>> {
    config
        .prepare_guest_package_env
        .map(|prepare| prepare(staging_root))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn prepare_host_cross_build_env(
    arch: &str,
    layout: &case_assets::CaseAssetLayout,
    qemu_runner: &Path,
) -> anyhow::Result<HostCrossBuildEnv> {
    let spec = cross_compile_spec(arch)?;
    let cmake = find_host_binary_candidates(&["cmake"])?;
    let clang = find_host_binary_candidates(&["clang"])?;
    let pkg_config = find_host_binary_candidates(&["pkg-config"])?;
    let make_program = find_host_binary_candidates(&["make", "gmake"])?;

    write_cross_bin_wrappers(layout, spec, qemu_runner)?;
    write_cmake_toolchain_file(layout, spec, &clang)?;

    let pkgconfig_libdir = format!(
        "{}:{}",
        layout.staging_root.join("usr/lib/pkgconfig").display(),
        layout.staging_root.join("usr/share/pkgconfig").display()
    );
    let command_envs = vec![
        ("PKG_CONFIG_LIBDIR".to_string(), pkgconfig_libdir),
        (
            "PKG_CONFIG_SYSROOT_DIR".to_string(),
            layout.staging_root.display().to_string(),
        ),
        ("PKG_CONFIG_PATH".to_string(), String::new()),
    ];

    Ok(HostCrossBuildEnv {
        cmake,
        pkg_config,
        make_program,
        cmake_toolchain_file: layout.cmake_toolchain_file.clone(),
        command_envs,
    })
}

pub(crate) fn cross_compile_spec(arch: &str) -> anyhow::Result<CrossCompileSpec> {
    crate::context::cross_compile_spec_for_arch_checked(arch)
}

pub(crate) fn write_cross_bin_wrappers(
    layout: &case_assets::CaseAssetLayout,
    spec: CrossCompileSpec,
    qemu_runner: &Path,
) -> anyhow::Result<()> {
    fs::create_dir_all(&layout.cross_bin_dir)
        .with_context(|| format!("failed to create {}", layout.cross_bin_dir.display()))?;
    for tool in CROSS_BINUTILS {
        let guest_relative_path = format!("{}/{tool}", spec.guest_tool_dir);
        ensure_guest_tool_exists(&layout.staging_root, &guest_relative_path)?;
        write_guest_exec_wrapper(
            &layout.cross_bin_dir.join(tool),
            qemu_runner,
            &layout.staging_root,
            &guest_relative_path,
            None,
        )?;
        write_guest_exec_wrapper(
            &layout
                .cross_bin_dir
                .join(format!("{}-{tool}", spec.gnu_tool_prefix)),
            qemu_runner,
            &layout.staging_root,
            &guest_relative_path,
            None,
        )?;
    }

    Ok(())
}

pub(crate) fn write_cmake_toolchain_file(
    layout: &case_assets::CaseAssetLayout,
    spec: CrossCompileSpec,
    clang: &Path,
) -> anyhow::Result<()> {
    if let Some(parent) = layout.cmake_toolchain_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let sysroot = &layout.staging_root;
    let gcc_toolchain_root = sysroot.join("usr");
    let mut compile_flags = vec![
        format!("--sysroot={}", sysroot.display()),
        format!("--gcc-toolchain={}", gcc_toolchain_root.display()),
        format!("-B{}", layout.cross_bin_dir.display()),
    ];
    let mut linker_flags = compile_flags.clone();
    if let Some(gcc_runtime_dir) = detect_gcc_runtime_dir(sysroot, spec.guest_tool_dir) {
        // Older host clang may miss Alpine GCC runtime dirs unless explicitly provided.
        compile_flags.push(format!("-B{}", gcc_runtime_dir.display()));
        linker_flags = compile_flags.clone();
        linker_flags.push(format!("-L{}", gcc_runtime_dir.display()));
    }
    let compile_flags = compile_flags.join(" ");
    let linker_flags = linker_flags.join(" ");

    let mut content = include_str!("cmake-toolchain.cmake.in").to_string();
    for (needle, value) in [
        (
            "@CMAKE_SYSTEM_PROCESSOR@",
            spec.cmake_system_processor.to_string(),
        ),
        ("@CMAKE_SYSROOT@", cmake_value(sysroot)),
        ("@CMAKE_FIND_ROOT_PATH@", cmake_value(sysroot)),
        ("@CMAKE_C_COMPILER@", cmake_value(clang)),
        ("@CMAKE_C_COMPILER_TARGET@", spec.llvm_target.to_string()),
        ("@CMAKE_ASM_COMPILER@", cmake_value(clang)),
        ("@CMAKE_ASM_COMPILER_TARGET@", spec.llvm_target.to_string()),
        ("@CMAKE_AR@", cmake_value(layout.cross_bin_dir.join("ar"))),
        (
            "@CMAKE_RANLIB@",
            cmake_value(layout.cross_bin_dir.join("ranlib")),
        ),
        (
            "@CMAKE_STRIP@",
            cmake_value(layout.cross_bin_dir.join("strip")),
        ),
        (
            "@CMAKE_LINKER@",
            cmake_value(layout.cross_bin_dir.join("ld")),
        ),
        ("@CMAKE_NM@", cmake_value(layout.cross_bin_dir.join("nm"))),
        (
            "@CMAKE_OBJCOPY@",
            cmake_value(layout.cross_bin_dir.join("objcopy")),
        ),
        (
            "@CMAKE_OBJDUMP@",
            cmake_value(layout.cross_bin_dir.join("objdump")),
        ),
        (
            "@CMAKE_READELF@",
            cmake_value(layout.cross_bin_dir.join("readelf")),
        ),
        (
            "@CMAKE_C_COMPILER_AR@",
            cmake_value(layout.cross_bin_dir.join("ar")),
        ),
        (
            "@CMAKE_C_COMPILER_RANLIB@",
            cmake_value(layout.cross_bin_dir.join("ranlib")),
        ),
        ("@CMAKE_C_FLAGS_INIT@", cmake_value(&compile_flags)),
        ("@CMAKE_ASM_FLAGS_INIT@", cmake_value(&compile_flags)),
        ("@CMAKE_LINKER_FLAGS_INIT@", cmake_value(&linker_flags)),
    ] {
        content = content.replace(needle, &value);
    }

    fs::write(&layout.cmake_toolchain_file, content)
        .with_context(|| format!("failed to write {}", layout.cmake_toolchain_file.display()))
}

fn cmake_value(value: impl AsRef<std::ffi::OsStr>) -> String {
    value.as_ref().to_string_lossy().replace('\\', "/")
}

fn detect_gcc_runtime_dir(sysroot: &Path, guest_tool_dir: &str) -> Option<PathBuf> {
    let triplet = Path::new(guest_tool_dir).parent()?.file_name()?;
    let gcc_root = sysroot.join("usr/lib/gcc").join(triplet);
    let entries = fs::read_dir(&gcc_root).ok()?;
    let runtime_dirs = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();

    runtime_dirs
        .iter()
        .filter_map(|path| {
            let dir_name = path.file_name()?.to_str()?;
            let version = parse_gcc_runtime_version(dir_name)?;
            Some((version, path))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)))
        .map(|(_, path)| path.clone())
        .or_else(|| runtime_dirs.into_iter().max())
}

fn parse_gcc_runtime_version(dir_name: &str) -> Option<Vec<u64>> {
    let mut version = Vec::new();
    for segment in dir_name.split('.') {
        if segment.is_empty() {
            return None;
        }
        let digits = segment
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if digits.is_empty() {
            return None;
        }
        version.push(digits.parse().ok()?);
    }
    Some(version)
}

pub(crate) fn build_prebuild_command(
    case: &TestQemuCase,
    prebuild_script: &Path,
    layout: &case_assets::CaseAssetLayout,
    prebuild_env: &GuestPrebuildEnv,
) -> anyhow::Result<Command> {
    build_prebuild_command_with_work_dir(
        prebuild_script,
        &case_c_source_dir(case),
        layout,
        prebuild_env,
    )
}

fn build_prebuild_command_with_work_dir(
    prebuild_script: &Path,
    work_dir: &Path,
    layout: &case_assets::CaseAssetLayout,
    prebuild_env: &GuestPrebuildEnv,
) -> anyhow::Result<Command> {
    let guest_busybox = layout.staging_root.join("bin/busybox");
    let guest_shell = layout.staging_root.join("bin/sh");
    let mut command = Command::new(&prebuild_env.qemu_runner);
    command.arg("-L").arg(&layout.staging_root);
    if guest_busybox.is_file() {
        command.arg(&guest_busybox).arg("sh");
    } else {
        ensure!(
            guest_shell.is_file(),
            "staging root is missing guest shell `{}`",
            guest_shell.display()
        );
        command.arg(&guest_shell);
    }
    command
        .arg("-eu")
        .arg(prebuild_script)
        .current_dir(work_dir);
    apply_case_script_envs(&mut command, layout, &prebuild_env.script_envs)?;
    Ok(command)
}

pub(crate) fn build_cmake_configure_command(
    case: &TestQemuCase,
    layout: &case_assets::CaseAssetLayout,
    build_env: &HostCrossBuildEnv,
    config: &CaseAssetConfig,
) -> Command {
    build_cmake_configure_command_with_source_dir(
        &case_c_source_dir(case),
        layout,
        build_env,
        config,
    )
}

fn build_cmake_configure_command_with_source_dir(
    source_dir: &Path,
    layout: &case_assets::CaseAssetLayout,
    build_env: &HostCrossBuildEnv,
    config: &CaseAssetConfig,
) -> Command {
    let mut command = Command::new(&build_env.cmake);
    command
        .arg("-S")
        .arg(source_dir)
        .arg("-B")
        .arg(&layout.build_dir)
        .arg("-G")
        .arg("Unix Makefiles")
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_INSTALL_PREFIX=/")
        .arg("-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY")
        .arg(format!(
            "-DCMAKE_TOOLCHAIN_FILE={}",
            build_env.cmake_toolchain_file.display()
        ))
        .arg(format!(
            "-DCMAKE_MAKE_PROGRAM={}",
            build_env.make_program.display()
        ))
        .arg(format!(
            "-DPKG_CONFIG_EXECUTABLE={}",
            build_env.pkg_config.display()
        ))
        .arg(format!(
            "-D{}={}",
            config.script_env.staging_root,
            layout.staging_root.display()
        ));

    for (key, value) in &build_env.command_envs {
        command.env(key, value);
    }

    command
}

fn build_grouped_c_root_project_configure_command(
    case: &TestQemuCase,
    selected_subcases: &[&TestQemuSubcase],
    all_c_subcase_count: usize,
    layout: &case_assets::CaseAssetLayout,
    build_env: &HostCrossBuildEnv,
    config: &CaseAssetConfig,
) -> Command {
    let mut command =
        build_cmake_configure_command_with_source_dir(&case.case_dir, layout, build_env, config);
    if let Some(subcases) =
        grouped_c_root_project_selected_subcase_define(case, selected_subcases, all_c_subcase_count)
    {
        command.arg(format!("-DSTARRY_GROUPED_C_SUBCASES={subcases}"));
    }
    command
}

fn grouped_c_root_project_selected_subcase_define(
    case: &TestQemuCase,
    selected_subcases: &[&TestQemuSubcase],
    all_c_subcase_count: usize,
) -> Option<String> {
    let filter_nonempty = case
        .grouped_subcase_filter
        .as_ref()
        .is_some_and(|filter| !filter.is_empty());
    if selected_subcases.len() >= all_c_subcase_count && !filter_nonempty {
        return None;
    }

    let mut names = selected_subcases
        .iter()
        .map(|subcase| subcase.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    Some(names.join(";"))
}

fn build_cmake_build_command(
    layout: &case_assets::CaseAssetLayout,
    build_env: &HostCrossBuildEnv,
) -> Command {
    let mut command = Command::new(&build_env.cmake);
    command
        .arg("--build")
        .arg(&layout.build_dir)
        .arg("--parallel");

    for (key, value) in &build_env.command_envs {
        command.env(key, value);
    }

    command
}

fn build_cmake_install_command(
    layout: &case_assets::CaseAssetLayout,
    build_env: &HostCrossBuildEnv,
) -> Command {
    let mut command = Command::new(&build_env.cmake);
    command.arg("--install").arg(&layout.build_dir);
    command.env("DESTDIR", &layout.overlay_dir);

    for (key, value) in &build_env.command_envs {
        command.env(key, value);
    }

    command
}

fn apply_case_script_envs(
    command: &mut Command,
    layout: &case_assets::CaseAssetLayout,
    script_envs: &[(String, String)],
) -> anyhow::Result<()> {
    let host_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = Vec::new();
    path_entries.push(layout.command_wrapper_dir.clone());
    path_entries.extend(std::env::split_paths(&host_path));

    let path = std::env::join_paths(path_entries)
        .map_err(|e| anyhow::anyhow!("failed to build case script PATH: {e}"))?;
    command.env("PATH", path);
    command.env("QEMU_LD_PREFIX", &layout.staging_root);
    command.env("LD_LIBRARY_PATH", guest_library_path(&layout.staging_root));

    for (key, value) in script_envs {
        command.env(key, value);
    }

    Ok(())
}

pub(crate) fn case_script_envs(
    case: &TestQemuCase,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
) -> Vec<(String, String)> {
    vec![
        (
            config.script_env.staging_root.clone(),
            layout.staging_root.display().to_string(),
        ),
        (
            config.script_env.case_dir.clone(),
            case.case_dir.display().to_string(),
        ),
        (
            config.script_env.case_c_dir.clone(),
            case_c_source_dir(case).display().to_string(),
        ),
        (
            config.script_env.case_work_dir.clone(),
            layout.work_dir.display().to_string(),
        ),
        (
            config.script_env.case_build_dir.clone(),
            layout.build_dir.display().to_string(),
        ),
        (
            config.script_env.case_overlay_dir.clone(),
            layout.overlay_dir.display().to_string(),
        ),
    ]
}

fn write_guest_command_wrappers(
    layout: &case_assets::CaseAssetLayout,
    qemu_runner: &Path,
) -> anyhow::Result<()> {
    let mut guest_commands = BTreeMap::new();
    for relative_dir in ["bin", "sbin", "usr/bin", "usr/sbin"] {
        let dir_path = layout.staging_root.join(relative_dir);
        if !dir_path.is_dir() {
            continue;
        }

        let mut entries = fs::read_dir(&dir_path)
            .with_context(|| format!("failed to read {}", dir_path.display()))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read {}", dir_path.display()))?;
        entries.sort_by_key(|left| left.file_name());

        for entry in entries {
            let name = entry.file_name();
            if guest_commands.contains_key(name.as_os_str()) {
                continue;
            }

            let file_type = entry.file_type().with_context(|| {
                format!("failed to inspect guest command {}", entry.path().display())
            })?;
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }
            guest_commands.insert(
                name,
                format!("{relative_dir}/{}", entry.file_name().to_string_lossy()),
            );
        }
    }

    for (name, relative_guest_path) in guest_commands {
        let wrapper_path = layout.command_wrapper_dir.join(&name);
        if relative_guest_path == "sbin/apk" {
            write_apk_wrapper_script(&wrapper_path, qemu_runner, &layout.staging_root, layout)?;
        } else {
            write_guest_exec_wrapper(
                &wrapper_path,
                qemu_runner,
                &layout.staging_root,
                &relative_guest_path,
                None,
            )?;
        }
    }

    Ok(())
}

fn ensure_guest_tool_exists(staging_root: &Path, relative_path: &str) -> anyhow::Result<()> {
    let path = staging_root.join(relative_path);
    ensure!(
        path.is_file(),
        "staging root is missing required guest tool `{}`; install it in prebuild.sh",
        path.display()
    );
    Ok(())
}

pub(crate) fn qemu_user_binary_names(arch: &str) -> anyhow::Result<&'static [&'static str]> {
    Ok(cross_compile_spec(arch)?.qemu_user_binaries)
}

fn write_guest_exec_wrapper(
    path: &Path,
    qemu_runner: &Path,
    staging_root: &Path,
    guest_relative_path: &str,
    extra_args: Option<String>,
) -> anyhow::Result<()> {
    let guest_path = staging_root.join(guest_relative_path);
    let mut body = format!(
        "export QEMU_LD_PREFIX={root}\nexport LD_LIBRARY_PATH={lib_path}\nexec {qemu} -0 {guest} \
         -L {root} {guest}",
        root = shell_single_quote(staging_root),
        lib_path = shell_single_quote(guest_library_path(staging_root)),
        qemu = shell_single_quote(qemu_runner),
        guest = shell_single_quote(&guest_path),
    );
    if let Some(extra_args) = extra_args {
        body.push(' ');
        body.push_str(&extra_args);
    }
    body.push_str(" \"$@\"\n");

    write_wrapper_script(path, &body)
}

fn write_apk_wrapper_script(
    path: &Path,
    qemu_runner: &Path,
    staging_root: &Path,
    layout: &case_assets::CaseAssetLayout,
) -> anyhow::Result<()> {
    let body = format!(
        "export QEMU_LD_PREFIX={root}\nexport LD_LIBRARY_PATH={lib_path}\nexec {qemu} -L {root} \
         {apk} --root {root} --repositories-file {repositories} --keys-dir {keys} --cache-dir \
         {cache} --update-cache --timeout 60 --no-interactive --force-no-chroot --scripts=no \
         \"$@\"\n",
        root = shell_single_quote(staging_root),
        lib_path = shell_single_quote(guest_library_path(staging_root)),
        qemu = shell_single_quote(qemu_runner),
        apk = shell_single_quote(staging_root.join("sbin/apk")),
        repositories = shell_single_quote(staging_root.join("etc/apk/repositories")),
        keys = shell_single_quote(staging_root.join("etc/apk/keys")),
        cache = shell_single_quote(&layout.apk_cache_dir),
    );
    write_wrapper_script(path, &body)
}

fn guest_library_path(staging_root: &Path) -> String {
    format!(
        "{}:{}",
        staging_root.join("lib").display(),
        staging_root.join("usr/lib").display()
    )
}

fn write_wrapper_script(path: &Path, body: &str) -> anyhow::Result<()> {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}"))
        .with_context(|| format!("failed to write {}", path.display()))?;
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

fn find_host_binary_candidates(candidates: &[&str]) -> anyhow::Result<PathBuf> {
    candidates
        .iter()
        .find_map(|candidate| find_optional_host_binary(candidate))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "required host binary was not found in PATH; tried: {}",
                candidates.join(", ")
            )
        })
}

fn find_optional_host_binary(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn shell_single_quote(path: impl AsRef<Path>) -> String {
    let value = path.as_ref().display().to_string().replace('\'', "'\\''");
    format!("'{value}'")
}

/// Returns the Rust source directory for a QEMU test case.
pub(crate) fn case_rust_source_dir(case: &TestQemuCase) -> PathBuf {
    if case.case_dir.join("Cargo.toml").is_file() {
        case.case_dir.clone()
    } else {
        case.case_dir.join("rust")
    }
}

/// Maps a StarryOS arch name to the corresponding Rust musl target triple.
fn rust_musl_target(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "aarch64" => Ok("aarch64-unknown-linux-musl"),
        "riscv64" => Ok("riscv64gc-unknown-linux-musl"),
        "x86_64" => Ok("x86_64-unknown-linux-musl"),
        "loongarch64" => Ok("loongarch64-unknown-linux-musl"),
        _ => bail!(
            "Rust-based QEMU test cases are only supported on aarch64, riscv64, x86_64, and \
             loongarch64, but got `{arch}`"
        ),
    }
}

/// Prepares overlay assets for a Rust-based QEMU test case.
///
/// This pipeline:
/// 1. Cross-compiles the Rust project in the case root or `rust/` using
///    `cargo build --release` targeting the appropriate musl triple for the
///    guest architecture.
/// 2. Copies the resulting static binary into the overlay at `/usr/bin/`.
/// 3. Injects the overlay into the rootfs image.
///
/// The binary name is taken from the Cargo.toml `[[bin]]` name, or falls back
/// to the package name.  The case root or `rust/` directory must contain a
/// `Cargo.toml`.
pub(crate) fn prepare_rust_case_assets_sync(
    arch: &str,
    case: &TestQemuCase,
    case_rootfs: &Path,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
) -> anyhow::Result<()> {
    let rust_dir = case_rust_source_dir(case);
    ensure!(
        rust_dir.is_dir(),
        "missing case Rust source directory `{}`",
        rust_dir.display()
    );
    let cargo_toml = rust_dir.join("Cargo.toml");
    ensure!(
        cargo_toml.is_file(),
        "missing Cargo.toml in Rust case source directory `{}`",
        rust_dir.display()
    );

    let target_triple = rust_musl_target(arch)?;

    case_assets::reset_dir(&layout.overlay_dir)?;
    case_assets::reset_dir(&layout.staging_root)?;
    case_assets::reset_dir(&layout.command_wrapper_dir)?;
    case_assets::reset_dir(&layout.cross_bin_dir)?;
    fs::create_dir_all(&layout.apk_cache_dir)
        .with_context(|| format!("failed to create {}", layout.apk_cache_dir.display()))?;

    // Ensure the musl target is installed in the active toolchain.
    let mut add_target = Command::new("rustup");
    add_target.arg("target").arg("add").arg(target_triple);
    add_target
        .exec()
        .with_context(|| format!("failed to install Rust target `{target_triple}` via rustup"))?;

    // Extract the rootfs so we can inject the compiled binary and, when needed,
    // use the Alpine cross-linker for architectures whose ELF format the host
    // linker cannot handle (e.g. loongarch64).
    crate::rootfs::inject::extract_rootfs(case_rootfs, &layout.staging_root)?;
    (config.prepare_staging_root)(&layout.staging_root)?;
    write_musl_loader_search_path(arch, &layout.staging_root)?;

    // Build qemu-user wrappers for Alpine binutils. Rust's musl targets have a
    // self-contained linker on common arches; keep Alpine ld only where it is
    // still required.
    let spec = cross_compile_spec(arch)?;
    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    write_cross_bin_wrappers(layout, spec, &qemu_runner)?;

    // Run prebuild.sh if present — runs inside the Alpine staging root via
    // qemu-user, same as C cases.  Use this to install native deps (e.g.
    // `apk add dbus-dev`) that the cargo build needs via pkg-config.
    let prebuild_script = case_rust_prebuild_script_path(case);
    if prebuild_script.is_file() {
        let extra_script_envs = prepare_guest_package_env(config, &layout.staging_root)?;
        let prebuild_env =
            prepare_guest_prebuild_env(arch, case, layout, extra_script_envs, config)?;
        let mut command = build_prebuild_command(case, &prebuild_script, layout, &prebuild_env)?;
        // Override current_dir to rust/ — build_prebuild_command defaults to c/.
        command.current_dir(&rust_dir);
        command
            .exec()
            .with_context(|| format!("failed to run rust case prebuild.sh for `{}`", case.name))?;
    }

    // Cross-compile the Rust project for the musl target.
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--release")
        .arg("--target")
        .arg(target_triple)
        .arg("--manifest-path")
        .arg(&cargo_toml)
        .arg("--target-dir")
        .arg(&layout.build_dir)
        .env("RUSTFLAGS", "-C target-feature=+crt-static")
        // Point pkg-config at the Alpine sysroot so crates with native deps
        // (e.g. dbus via keyring) can find their .pc files when cross-compiling.
        .env(
            "PKG_CONFIG_LIBDIR",
            format!(
                "{}:{}",
                layout.staging_root.join("usr/lib/pkgconfig").display(),
                layout.staging_root.join("usr/share/pkgconfig").display()
            ),
        )
        .env(
            "PKG_CONFIG_SYSROOT_DIR",
            layout.staging_root.display().to_string(),
        )
        .env("PKG_CONFIG_PATH", "");
    if rust_case_needs_guest_linker(arch) {
        // The linker env var name is CARGO_TARGET_<UPPER_TRIPLE>_LINKER.
        let linker_env_key = format!(
            "CARGO_TARGET_{}_LINKER",
            target_triple.to_uppercase().replace('-', "_")
        );
        cmd.env(linker_env_key, layout.cross_bin_dir.join("ld"));
    }
    cmd.exec().with_context(|| {
        format!(
            "failed to cross-compile Rust case `{}` for target `{target_triple}`",
            case.name
        )
    })?;

    // Discover the binary name from Cargo.toml.
    let bin_name = rust_case_bin_name(&cargo_toml)?;

    // The compiled binary lives at <build_dir>/<target_triple>/release/<bin_name>.
    let bin_src = layout
        .build_dir
        .join(target_triple)
        .join("release")
        .join(&bin_name);
    ensure!(
        bin_src.is_file(),
        "expected compiled Rust binary at `{}` but it was not found",
        bin_src.display()
    );

    // Install the binary into the overlay at /usr/bin/.
    let dest_bin_dir = layout.overlay_dir.join("usr/bin");
    fs::create_dir_all(&dest_bin_dir)
        .with_context(|| format!("failed to create {}", dest_bin_dir.display()))?;
    let bin_dst = dest_bin_dir.join(&bin_name);
    fs::copy(&bin_src, &bin_dst).with_context(|| {
        format!(
            "failed to copy {} to {}",
            bin_src.display(),
            bin_dst.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin_dst)
            .with_context(|| format!("failed to stat {}", bin_dst.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin_dst, perms)
            .with_context(|| format!("failed to chmod {}", bin_dst.display()))?;
    }

    crate::rootfs::inject::inject_overlay(case_rootfs, &layout.overlay_dir)
}

fn rust_case_needs_guest_linker(arch: &str) -> bool {
    matches!(arch, "loongarch64")
}

/// Reads the binary name from a `Cargo.toml`.
///
/// Returns the first `[[bin]]` name if present, otherwise the `[package]` name.
fn rust_case_bin_name(cargo_toml: &Path) -> anyhow::Result<String> {
    #[derive(serde::Deserialize)]
    struct CargoToml {
        package: Option<CargoPackage>,
        bin: Option<Vec<CargoBin>>,
    }
    #[derive(serde::Deserialize)]
    struct CargoPackage {
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct CargoBin {
        name: String,
    }

    let content = fs::read_to_string(cargo_toml)
        .with_context(|| format!("failed to read {}", cargo_toml.display()))?;
    let manifest: CargoToml = toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", cargo_toml.display()))?;

    if let Some(bins) = manifest.bin
        && let Some(first) = bins.into_iter().next()
    {
        return Ok(first.name);
    }

    manifest.package.map(|p| p.name).ok_or_else(|| {
        anyhow::anyhow!(
            "no `[package]` or `[[bin]]` found in {}",
            cargo_toml.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, fs, path::PathBuf, time::Duration};

    use tempfile::tempdir;

    use super::*;

    fn fake_config() -> CaseAssetConfig {
        CaseAssetConfig {
            grouped_runner: case_assets::GroupedCaseRunnerConfig {
                runner_name: "suite-run-case-tests".to_string(),
                runner_path: "/usr/bin/suite-run-case-tests".to_string(),
                autorun_profile_script: None,
                begin_marker: "SUITE_GROUPED_TEST_BEGIN".to_string(),
                passed_marker: "SUITE_GROUPED_TEST_PASSED".to_string(),
                failed_marker: "SUITE_GROUPED_TEST_FAILED".to_string(),
                all_passed_marker: "SUITE_GROUPED_TESTS_PASSED".to_string(),
                all_failed_marker: "SUITE_GROUPED_TESTS_FAILED".to_string(),
                success_regex: r"(?m)^SUITE_GROUPED_TESTS_PASSED\s*$".to_string(),
                fail_regex: r"(?m)^SUITE_GROUPED_TEST_FAILED:".to_string(),
            },
            script_env: case_assets::CaseScriptEnvConfig {
                staging_root: "SUITE_STAGING_ROOT".to_string(),
                case_dir: "SUITE_CASE_DIR".to_string(),
                case_c_dir: "SUITE_CASE_C_DIR".to_string(),
                case_work_dir: "SUITE_CASE_WORK_DIR".to_string(),
                case_build_dir: "SUITE_CASE_BUILD_DIR".to_string(),
                case_overlay_dir: "SUITE_CASE_OVERLAY_DIR".to_string(),
            },
            cache_env_vars: vec!["SUITE_PACKAGE_REGION".to_string()],
            prepare_staging_root: |_| Ok(()),
            prepare_guest_package_env: Some(|_| {
                Ok(vec![("SUITE_PACKAGE_REGION".to_string(), "us".to_string())])
            }),
        }
    }

    fn fake_case(root: &Path, name: &str) -> TestQemuCase {
        let case_dir = root.join("test-suite/example/default").join(name);
        fs::create_dir_all(&case_dir).unwrap();
        TestQemuCase {
            name: name.to_string(),
            display_name: name.to_string(),
            case_dir: case_dir.clone(),
            qemu_config_path: case_dir.join("qemu-aarch64.toml"),
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server: None,
            subcases: Vec::new(),
            grouped_subcase_filter: None,
        }
    }

    fn fake_c_subcase(
        root: &Path,
        case: &TestQemuCase,
        name: &str,
        install_targets: &[&str],
    ) -> TestQemuSubcase {
        let case_dir = case.case_dir.join(name);
        let c_dir = case_dir.join("c");
        fs::create_dir_all(&c_dir).unwrap();
        fs::write(
            c_dir.join(CASE_CMAKE_FILE_NAME),
            format!(
                "add_executable({target} src/main.c)\ninstall(TARGETS {} RUNTIME DESTINATION \
                 usr/bin)\n",
                install_targets.join(" "),
                target = install_targets.first().unwrap_or(&name)
            ),
        )
        .unwrap();

        assert!(case_dir.starts_with(root));
        TestQemuSubcase {
            name: name.to_string(),
            case_dir,
            kind: TestQemuSubcaseKind::C,
        }
    }

    fn command_env(command: &Command, key: &str) -> Option<String> {
        command.get_envs().find_map(|(name, value)| {
            (name == OsStr::new(key))
                .then(|| value.map(|value| value.to_string_lossy().into_owned()))
                .flatten()
        })
    }

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn write_musl_loader_search_path_uses_requested_guest_arch() {
        let root = tempdir().unwrap();
        let staging_root = root.path().join("staging-root");
        fs::create_dir_all(staging_root.join("lib")).unwrap();
        fs::write(staging_root.join("lib/ld-musl-riscv64.so.1"), b"").unwrap();

        write_musl_loader_search_path("riscv64", &staging_root).unwrap();

        assert_eq!(
            fs::read_to_string(staging_root.join("etc/ld-musl-riscv64.path")).unwrap(),
            "/usr/lib\n/lib\n"
        );
        assert!(!staging_root.join("etc/ld-musl-aarch64.path").exists());
    }

    #[test]
    fn write_musl_loader_search_path_skips_when_guest_loader_is_missing() {
        let root = tempdir().unwrap();
        let staging_root = root.path().join("staging-root");
        fs::create_dir_all(staging_root.join("lib")).unwrap();
        fs::write(staging_root.join("lib/ld-musl-riscv64.so.1"), b"").unwrap();

        write_musl_loader_search_path("aarch64", &staging_root).unwrap();

        assert!(!staging_root.join("etc/ld-musl-aarch64.path").exists());
        assert!(!staging_root.join("etc/ld-musl-riscv64.path").exists());
    }

    #[test]
    fn build_prebuild_command_uses_guest_shell_and_case_envs() {
        let root = tempdir().unwrap();
        let case = fake_case(root.path(), "usb");
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
                .unwrap();
        fs::create_dir_all(layout.staging_root.join("bin")).unwrap();
        fs::write(layout.staging_root.join("bin/sh"), b"").unwrap();
        fs::write(layout.staging_root.join("bin/busybox"), b"").unwrap();
        let prebuild_env = GuestPrebuildEnv {
            qemu_runner: PathBuf::from("/usr/bin/qemu-aarch64-static"),
            script_envs: {
                let mut envs = case_script_envs(&case, &layout, &fake_config());
                envs.push(("SUITE_PACKAGE_REGION".to_string(), "us".to_string()));
                envs
            },
        };
        let prebuild_script = case_c_source_dir(&case).join("prebuild.sh");

        let command =
            build_prebuild_command(&case, &prebuild_script, &layout, &prebuild_env).unwrap();

        assert_eq!(
            command.get_program(),
            std::ffi::OsStr::new("/usr/bin/qemu-aarch64-static")
        );
        assert_eq!(
            command_args(&command),
            vec![
                "-L".to_string(),
                layout.staging_root.display().to_string(),
                layout
                    .staging_root
                    .join("bin/busybox")
                    .display()
                    .to_string(),
                "sh".to_string(),
                "-eu".to_string(),
                prebuild_script.display().to_string(),
            ]
        );
        assert_eq!(
            command.get_current_dir(),
            Some(case_c_source_dir(&case).as_path())
        );
        assert_eq!(
            command_env(&command, "SUITE_CASE_OVERLAY_DIR"),
            Some(layout.overlay_dir.display().to_string())
        );
        assert_eq!(
            command_env(&command, "SUITE_PACKAGE_REGION"),
            Some("us".to_string())
        );
        assert_eq!(
            command_env(&command, "LD_LIBRARY_PATH"),
            Some(guest_library_path(&layout.staging_root))
        );
    }

    #[test]
    fn grouped_c_subcases_keep_only_direct_usr_bin_commands() {
        let root = tempdir().unwrap();
        let mut case = fake_case(root.path(), "bugfix");
        case.test_commands = vec![
            "/usr/bin/alpha".to_string(),
            "/usr/bin/gamma --stress".to_string(),
        ];

        let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
        let beta = fake_c_subcase(root.path(), &case, "beta", &["beta"]);
        let gamma = fake_c_subcase(root.path(), &case, "gamma-dir", &["gamma"]);
        let subcases = vec![&alpha, &beta, &gamma];

        let selected = selected_grouped_c_subcases(&case, subcases).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|subcase| subcase.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "gamma-dir"]
        );
    }

    #[test]
    fn grouped_c_subcases_keep_all_dynamic_shell_commands() {
        let root = tempdir().unwrap();
        let mut case = fake_case(root.path(), "syscall");
        case.test_commands =
            vec!["for bin in /usr/bin/starry-test-suit/*; do \"$bin\"; done".to_string()];

        let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
        let beta = fake_c_subcase(root.path(), &case, "beta", &["beta"]);
        let subcases = vec![&alpha, &beta];

        let selected = selected_grouped_c_subcases(&case, subcases).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|subcase| subcase.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn grouped_c_subcases_prefer_explicit_filter() {
        let root = tempdir().unwrap();
        let mut case = fake_case(root.path(), "syscall");
        case.test_commands =
            vec!["for bin in /usr/bin/starry-test-suit/*; do \"$bin\"; done".to_string()];
        case.grouped_subcase_filter = Some(BTreeSet::from(["beta".to_string()]));

        let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
        let beta = fake_c_subcase(root.path(), &case, "beta", &["beta"]);
        let subcases = vec![&alpha, &beta];

        let selected = selected_grouped_c_subcases(&case, subcases).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|subcase| subcase.name.as_str())
                .collect::<Vec<_>>(),
            vec!["beta"]
        );
    }

    #[test]
    fn grouped_runner_commands_follow_explicit_subcase_filter_for_direct_commands() {
        let root = tempdir().unwrap();
        let mut case = fake_case(root.path(), "bugfix");
        case.test_commands = vec![
            "/usr/bin/alpha".to_string(),
            "/usr/bin/beta --stress".to_string(),
        ];
        case.grouped_subcase_filter = Some(BTreeSet::from(["beta-dir".to_string()]));

        let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
        let beta = fake_c_subcase(root.path(), &case, "beta-dir", &["beta"]);
        let selected = selected_grouped_c_subcases(&case, vec![&alpha, &beta]).unwrap();
        let runner_commands = selected_grouped_runner_commands(&case, &selected).unwrap();

        assert_eq!(runner_commands, vec!["/usr/bin/beta --stress"]);
    }

    #[test]
    fn grouped_runner_commands_keep_dynamic_shell_loop_with_explicit_filter() {
        let root = tempdir().unwrap();
        let mut case = fake_case(root.path(), "syscall");
        case.test_commands =
            vec!["for bin in /usr/bin/starry-test-suit/*; do \"$bin\"; done".to_string()];
        case.grouped_subcase_filter = Some(BTreeSet::from(["beta".to_string()]));

        let beta = fake_c_subcase(root.path(), &case, "beta", &["beta"]);
        let selected = selected_grouped_c_subcases(&case, vec![&beta]).unwrap();
        let runner_commands = selected_grouped_runner_commands(&case, &selected).unwrap();

        assert_eq!(runner_commands, case.test_commands);
    }

    #[test]
    fn grouped_c_subcases_reject_missing_direct_usr_bin_commands() {
        let root = tempdir().unwrap();
        let mut case = fake_case(root.path(), "bugfix");
        case.test_commands = vec!["/usr/bin/missing".to_string()];

        let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
        let err = selected_grouped_c_subcases(&case, vec![&alpha]).unwrap_err();

        assert!(
            err.to_string()
                .contains("references test command(s) without C subcases: missing")
        );
    }

    #[test]
    fn cmake_configure_command_passes_staging_root_define() {
        let root = tempdir().unwrap();
        let case = fake_case(root.path(), "usb");
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
                .unwrap();
        let build_env = HostCrossBuildEnv {
            cmake: PathBuf::from("/usr/bin/cmake"),
            pkg_config: PathBuf::from("/usr/bin/pkg-config"),
            make_program: PathBuf::from("/usr/bin/make"),
            cmake_toolchain_file: PathBuf::from("/tmp/cmake-toolchain.cmake"),
            command_envs: vec![("PKG_CONFIG_LIBDIR".to_string(), "/sysroot".to_string())],
        };

        let config = fake_config();
        let command = build_cmake_configure_command(&case, &layout, &build_env, &config);
        let args = command_args(&command);

        assert_eq!(
            command.get_program(),
            std::ffi::OsStr::new("/usr/bin/cmake")
        );
        assert!(args.contains(&format!(
            "-DCMAKE_TOOLCHAIN_FILE={}",
            build_env.cmake_toolchain_file.display()
        )));
        assert!(args.contains(&format!(
            "-D{}={}",
            config.script_env.staging_root,
            layout.staging_root.display()
        )));
        assert_eq!(
            command_env(&command, "PKG_CONFIG_LIBDIR"),
            Some("/sysroot".to_string())
        );
    }

    #[test]
    fn grouped_c_root_configure_command_passes_selected_subcase_list() {
        let root = tempdir().unwrap();
        let mut case = fake_case(root.path(), "bugfix");
        case.test_commands = vec!["/usr/bin/beta".to_string()];
        let alpha = fake_c_subcase(root.path(), &case, "alpha", &["alpha"]);
        let beta = fake_c_subcase(root.path(), &case, "beta-dir", &["beta"]);
        let subcases = [&alpha, &beta];
        let selected = selected_grouped_c_subcases(&case, subcases.to_vec()).unwrap();
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "bugfix")
                .unwrap();
        let build_env = HostCrossBuildEnv {
            cmake: PathBuf::from("/usr/bin/cmake"),
            pkg_config: PathBuf::from("/usr/bin/pkg-config"),
            make_program: PathBuf::from("/usr/bin/make"),
            cmake_toolchain_file: PathBuf::from("/tmp/cmake-toolchain.cmake"),
            command_envs: Vec::new(),
        };

        let command = build_grouped_c_root_project_configure_command(
            &case,
            &selected,
            subcases.len(),
            &layout,
            &build_env,
            &fake_config(),
        );
        let args = command_args(&command);

        assert!(args.contains(&"-DSTARRY_GROUPED_C_SUBCASES=beta-dir".to_string()));
    }

    #[test]
    fn cross_compile_spec_maps_supported_arches() {
        assert_eq!(
            cross_compile_spec("aarch64").unwrap(),
            CrossCompileSpec {
                llvm_target: "aarch64-linux-musl",
                cmake_system_processor: "aarch64",
                guest_tool_dir: "usr/aarch64-alpine-linux-musl/bin",
                gnu_tool_prefix: "aarch64-linux-musl",
                qemu_user_binaries: &["qemu-aarch64-static", "qemu-aarch64"],
            }
        );
        assert_eq!(
            cross_compile_spec("loongarch64").unwrap(),
            CrossCompileSpec {
                llvm_target: "loongarch64-linux-musl",
                cmake_system_processor: "loongarch64",
                guest_tool_dir: "usr/loongarch64-alpine-linux-musl/bin",
                gnu_tool_prefix: "loongarch64-linux-musl",
                qemu_user_binaries: &["qemu-loongarch64-static", "qemu-loongarch64"],
            }
        );
    }

    #[test]
    fn write_cross_bin_wrappers_generates_prefixed_and_plain_tools() {
        let root = tempdir().unwrap();
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
                .unwrap();
        fs::create_dir_all(
            layout
                .staging_root
                .join("usr/aarch64-alpine-linux-musl/bin"),
        )
        .unwrap();
        for tool in [
            "ld", "as", "ar", "ranlib", "strip", "nm", "objcopy", "objdump", "readelf",
        ] {
            let path = layout
                .staging_root
                .join("usr/aarch64-alpine-linux-musl/bin")
                .join(tool);
            fs::write(path, b"").unwrap();
        }

        write_cross_bin_wrappers(
            &layout,
            cross_compile_spec("aarch64").unwrap(),
            Path::new("/usr/bin/qemu-aarch64-static"),
        )
        .unwrap();

        let plain = fs::read_to_string(layout.cross_bin_dir.join("ld")).unwrap();
        let prefixed =
            fs::read_to_string(layout.cross_bin_dir.join("aarch64-linux-musl-ld")).unwrap();
        assert!(plain.contains("qemu-aarch64-static"));
        assert!(plain.contains("LD_LIBRARY_PATH"));
        assert!(plain.contains("usr/aarch64-alpine-linux-musl/bin/ld"));
        assert!(prefixed.contains("usr/aarch64-alpine-linux-musl/bin/ld"));
        assert!(prefixed.contains("-0"));
    }

    #[test]
    fn write_cmake_toolchain_file_contains_clang_cross_settings() {
        let root = tempdir().unwrap();
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
                .unwrap();
        fs::create_dir_all(&layout.cross_bin_dir).unwrap();
        fs::create_dir_all(
            layout
                .staging_root
                .join("usr/lib/gcc/aarch64-alpine-linux-musl/15.2.0"),
        )
        .unwrap();

        write_cmake_toolchain_file(
            &layout,
            cross_compile_spec("aarch64").unwrap(),
            Path::new("/usr/bin/clang"),
        )
        .unwrap();

        let content = fs::read_to_string(&layout.cmake_toolchain_file).unwrap();
        assert!(content.contains("set(CMAKE_SYSTEM_NAME Linux)"));
        assert!(content.contains("set(CMAKE_C_COMPILER \"/usr/bin/clang\")"));
        assert!(content.contains("set(CMAKE_C_COMPILER_TARGET \"aarch64-linux-musl\")"));
        assert!(content.contains("--gcc-toolchain="));
        assert!(content.contains("-B"));
        assert!(content.contains("-L"));
        assert!(content.contains("CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER"));
    }

    #[test]
    fn detect_gcc_runtime_dir_prefers_highest_version() {
        let root = tempdir().unwrap();
        let sysroot = root.path().join("sysroot");
        let gcc_root = sysroot.join("usr/lib/gcc/aarch64-alpine-linux-musl");
        fs::create_dir_all(gcc_root.join("9.5.0")).unwrap();
        fs::create_dir_all(gcc_root.join("15.2.0")).unwrap();

        let selected =
            detect_gcc_runtime_dir(&sysroot, "usr/aarch64-alpine-linux-musl/bin").unwrap();
        assert_eq!(selected, gcc_root.join("15.2.0"));
    }

    #[test]
    fn qemu_user_binary_names_cover_supported_arches() {
        assert_eq!(
            qemu_user_binary_names("aarch64").unwrap(),
            &["qemu-aarch64-static", "qemu-aarch64"]
        );
        assert_eq!(
            qemu_user_binary_names("riscv64").unwrap(),
            &["qemu-riscv64-static", "qemu-riscv64"]
        );
        assert_eq!(
            qemu_user_binary_names("x86_64").unwrap(),
            &["qemu-x86_64-static", "qemu-x86_64"]
        );
        assert_eq!(
            qemu_user_binary_names("loongarch64").unwrap(),
            &["qemu-loongarch64-static", "qemu-loongarch64"]
        );
    }

    #[test]
    fn case_script_envs_include_expected_paths() {
        let root = tempdir().unwrap();
        let case = fake_case(root.path(), "usb");
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
                .unwrap();

        let envs = case_script_envs(&case, &layout, &fake_config());

        assert!(envs.contains(&(
            "SUITE_CASE_DIR".to_string(),
            case.case_dir.display().to_string()
        )));
        assert!(envs.contains(&(
            "SUITE_CASE_BUILD_DIR".to_string(),
            layout.build_dir.display().to_string()
        )));
    }

    #[test]
    fn format_duration_like_summary_helpers_are_precise_enough() {
        assert_eq!(
            format!("{:.2}", Duration::from_millis(1250).as_secs_f64()),
            "1.25"
        );
    }
}
