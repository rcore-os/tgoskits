use super::*;

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

pub(super) fn build_cmake_configure_command_with_source_dir(
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

pub(super) fn build_grouped_c_root_project_configure_command(
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

pub(super) fn grouped_c_root_project_selected_subcase_define(
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

pub(super) fn build_cmake_build_command(
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

pub(super) fn build_cmake_install_command(
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
