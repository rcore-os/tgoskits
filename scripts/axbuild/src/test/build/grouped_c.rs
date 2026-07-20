use super::*;

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

pub(super) fn selected_grouped_c_subcases<'a>(
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

pub(super) fn selected_grouped_runner_commands(
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

pub(super) fn direct_usr_bin_command_names(commands: &[String]) -> Option<BTreeSet<String>> {
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

pub(super) fn grouped_c_subcase_binary_names(
    subcase: &TestQemuSubcase,
) -> anyhow::Result<BTreeSet<String>> {
    let mut names = BTreeSet::from([subcase.name.clone()]);
    let cmake_lists = grouped_c_subcase_source_dir(subcase).join(CASE_CMAKE_FILE_NAME);
    if cmake_lists.is_file() {
        let content = fs::read_to_string(&cmake_lists)
            .with_context(|| format!("failed to read {}", cmake_lists.display()))?;
        names.extend(cmake_install_target_names(&content));
    }
    Ok(names)
}

pub(super) fn cmake_install_target_names(content: &str) -> BTreeSet<String> {
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

pub(super) fn prepare_grouped_c_subcases_sync(
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

pub(super) fn prepare_grouped_c_root_project_sync(
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

pub(super) fn print_slowest_grouped_c_subcases(
    case: &TestQemuCase,
    mut timings: Vec<(String, Duration)>,
) {
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

pub(super) fn subcase_layout(
    layout: &case_assets::CaseAssetLayout,
    subcase_name: &str,
) -> case_assets::CaseAssetLayout {
    let mut layout = layout.clone();
    layout.build_dir = layout.build_dir.join(subcase_name);
    layout
}

pub(super) fn subcase_as_case(case: &TestQemuCase, subcase: &TestQemuSubcase) -> TestQemuCase {
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
