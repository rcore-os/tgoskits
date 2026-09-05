use super::*;

pub(super) fn prepare_guest_prebuild_env(
    case: &TestQemuCase,
    layout: &case_assets::CaseAssetLayout,
    qemu_runner: &Path,
    extra_script_envs: Vec<(String, String)>,
    config: &CaseAssetConfig,
) -> anyhow::Result<GuestPrebuildEnv> {
    write_guest_command_wrappers(layout, qemu_runner)?;

    let mut script_envs = case_script_envs(case, layout, config);
    script_envs.extend(extra_script_envs);

    Ok(GuestPrebuildEnv {
        qemu_runner: qemu_runner.to_path_buf(),
        script_envs,
    })
}

pub(super) fn prepare_guest_package_env(
    config: &CaseAssetConfig,
    staging_root: &Path,
) -> anyhow::Result<Vec<(String, String)>> {
    config
        .prepare_guest_package_env
        .map(|prepare| prepare(staging_root))
        .transpose()
        .map(Option::unwrap_or_default)
}

/// Announces a skipped `prebuild.sh` in native-tool mode: the script runs as
/// a guest process under qemu-user, so a host without qemu cannot execute it.
/// The skip is loud on purpose — subcases that depend on packages installed
/// by the prebuild fail at guest runtime and must be traceable to this line.
pub(super) fn prebuild_skipped_notice(case_name: &str) {
    println!(
        "prebuild.sh skipped for `{case_name}`: this host has no qemu-user, so guest commands \
         cannot execute; subcases needing prebuild-provided guest packages will fail at runtime"
    );
}

pub(super) fn prepare_host_cross_build_env(
    arch: &str,
    layout: &case_assets::CaseAssetLayout,
    execution: &GuestToolExecution,
) -> anyhow::Result<HostCrossBuildEnv> {
    let spec = cross_compile_spec(arch)?;
    let cmake = find_host_binary_candidates(&["cmake"])?;
    let clang = find_host_binary_candidates(&["clang"])?;
    let pkg_config = find_host_binary_candidates(&["pkg-config"])?;
    let make_program = find_host_binary_candidates(&["make", "gmake"])?;

    write_cross_bin_wrappers(layout, spec, execution)?;
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
