use std::{fs, path::Path};

use anyhow::{Context, ensure};
use ostool::run::qemu::QemuConfig;

use super::{
    shell::{make_executable, shell_single_quote, write_executable_script},
    types::{GroupedCaseRunnerConfig, TestQemuCase},
};

pub(crate) fn apply_grouped_qemu_config(
    qemu: &mut QemuConfig,
    case: &TestQemuCase,
    config: &GroupedCaseRunnerConfig,
) {
    if !case.is_grouped() {
        return;
    }

    if config.autorun_profile_script.is_none() {
        qemu.shell_init_cmd = Some(grouped_runner_shell_init_cmd(config));
    }
    qemu.success_regex = vec![config.success_regex.clone()];
    if !qemu
        .fail_regex
        .iter()
        .any(|regex| regex == &config.fail_regex)
    {
        qemu.fail_regex.push(config.fail_regex.clone());
    }
}

fn grouped_runner_shell_init_cmd(config: &GroupedCaseRunnerConfig) -> String {
    format!("exec {}", config.runner_path)
}

pub(crate) fn write_grouped_case_runner_script(
    overlay_dir: &Path,
    test_commands: &[String],
    config: &GroupedCaseRunnerConfig,
) -> anyhow::Result<()> {
    ensure!(
        !test_commands.is_empty(),
        "grouped qemu case has no test commands"
    );

    let dest_dir = overlay_dir.join("usr/bin");
    fs::create_dir_all(&dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;
    let runner_path = dest_dir.join(&config.runner_name);

    let mut body = String::new();
    body.push_str(&format!(
        "failed=0\ntotal={}\nstep=0\n",
        test_commands.len()
    ));
    for command in test_commands {
        let quoted = shell_single_quote(command);
        let command_label = shell_single_quote(command);
        let begin = shell_single_quote(&config.begin_marker);
        let passed = shell_single_quote(&config.passed_marker);
        let failed = shell_single_quote(&config.failed_marker);
        body.push_str(&format!(
            "step=$((step + 1))\nnow=$(date +%s 2>/dev/null || printf unknown)\nprintf '%s: \
             step=%s/%s epoch=%s command=%s\\n' {begin} \"$step\" \"$total\" \"$now\" \
             {command_label}\nif sh -c {quoted}; then\n\tnow=$(date +%s 2>/dev/null || printf \
             unknown)\n\tprintf '%s: step=%s/%s epoch=%s status=0 command=%s\\n' {passed} \
             \"$step\" \"$total\" \"$now\" {command_label}\nelse\n\tstatus=$?\n\tnow=$(date +%s \
             2>/dev/null || printf unknown)\n\tprintf '%s: step=%s/%s epoch=%s status=%s \
             command=%s\\n' {failed} \"$step\" \"$total\" \"$now\" \"$status\" \
             {command_label}\n\tfailed=1\nfi\n"
        ));
    }
    let all_passed = shell_single_quote(&config.all_passed_marker);
    let all_failed = shell_single_quote(&config.all_failed_marker);
    body.push_str(&format!(
        "if [ \"$failed\" -eq 0 ]; then\n\tprintf '%s\\n' {all_passed}\n\texit 0\nfi\nprintf \
         '%s\\n' {all_failed}\nexit 1\n"
    ));

    write_executable_script(&runner_path, &body)?;
    if let Some(script_name) = &config.autorun_profile_script {
        write_grouped_case_autorun_profile_script(overlay_dir, script_name, &config.runner_path)?;
    }
    Ok(())
}

fn write_grouped_case_autorun_profile_script(
    overlay_dir: &Path,
    script_name: &str,
    runner_path: &str,
) -> anyhow::Result<()> {
    ensure!(
        !script_name.is_empty() && !script_name.contains('/') && script_name.ends_with(".sh"),
        "invalid grouped qemu autorun profile script name `{script_name}`"
    );

    let dest_dir = overlay_dir.join("etc/profile.d");
    fs::create_dir_all(&dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;
    let script_path = dest_dir.join(script_name);
    let runner = shell_single_quote(runner_path);
    let body = format!(
        "if [ \"${{AXBUILD_GROUPED_AUTORUN_DONE:-0}}\" = \"1\" ]; then\n\treturn 0 2>/dev/null || \
         exit 0\nfi\nexport AXBUILD_GROUPED_AUTORUN_DONE=1\n\nif [ -x {runner} ]; \
         then\n\t{runner}\nfi\n"
    );
    fs::write(&script_path, body)
        .with_context(|| format!("failed to write {}", script_path.display()))?;
    make_executable(&script_path)
}
