use super::*;

pub(crate) struct GuestPrebuildRequest<'a> {
    pub(crate) arch: &'a str,
    pub(crate) case: &'a TestQemuCase,
    pub(crate) case_rootfs: &'a Path,
    pub(crate) script: &'a Path,
    pub(crate) work_dir: &'a Path,
    pub(crate) layout: &'a case_assets::CaseAssetLayout,
    pub(crate) extra_envs: &'a [(String, String)],
    pub(crate) config: &'a CaseAssetConfig,
}

pub(crate) fn run_guest_prebuild(request: GuestPrebuildRequest<'_>) -> anyhow::Result<()> {
    let spec = cross_compile_spec(request.arch)?;
    if let Some(qemu_runner) = find_cross_tool_qemu(spec, find_optional_host_binary) {
        let prebuild_env = prepare_guest_prebuild_env_with_runner(
            request.case,
            request.layout,
            &qemu_runner,
            request.extra_envs.to_vec(),
            request.config,
        )?;
        return build_prebuild_command_with_work_dir(
            request.script,
            request.work_dir,
            request.layout,
            &prebuild_env,
        )?
        .exec();
    }

    system_prebuild::run_system_guest_prebuild(&request)
}

pub(super) fn build_prebuild_command_with_work_dir(
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
