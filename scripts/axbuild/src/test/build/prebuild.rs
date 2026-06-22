use super::*;

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
