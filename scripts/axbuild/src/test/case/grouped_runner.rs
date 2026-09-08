use std::{fs, path::Path};

use anyhow::{Context, ensure};
use ostool::run::qemu::QemuConfig;
use sha2::{Digest, Sha256};

use super::{
    shell::{shell_single_quote, write_executable_script},
    types::{GroupedCaseExecution, GroupedCaseRunnerConfig, TestQemuCase},
};

pub(crate) fn apply_grouped_qemu_config(
    qemu: &mut QemuConfig,
    case: &TestQemuCase,
    execution: &GroupedCaseExecution,
) {
    if !case.is_grouped() {
        return;
    }

    let Some(config) = execution.runner() else {
        return;
    };
    if matches!(execution, GroupedCaseExecution::ShellCommand(_)) {
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

pub(crate) fn write_grouped_case_runner(
    overlay_dir: &Path,
    test_commands: &[String],
    execution: &GroupedCaseExecution,
) -> anyhow::Result<()> {
    let Some(config) = execution.runner() else {
        return Ok(());
    };
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
        let command_label = shell_single_quote(&grouped_command_label(command));
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
    Ok(())
}

fn grouped_command_label(command: &str) -> String {
    let trimmed = command.trim();
    if !trimmed.contains('\n') && trimmed.len() <= 120 {
        return trimmed.to_string();
    }

    let mut hasher = Sha256::new();
    hasher.update(trimmed.len().to_le_bytes());
    hasher.update(trimmed.as_bytes());
    let digest = hasher.finalize();
    format!(
        "inline-command:{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}
