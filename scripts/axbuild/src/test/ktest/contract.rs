use super::*;

pub(super) fn apply_axtest_qemu_markers(qemu: &mut QemuConfig) {
    ensure_regex(&mut qemu.fail_regex, PANIC_FAIL);
    ensure_regex(&mut qemu.fail_regex, AXTEST_SUITE_FAIL);
    ensure_regex(&mut qemu.fail_regex, AXTEST_CASE_FAIL);
    require_final_suite_gate(&mut qemu.shell_check_steps);
}


pub(super) fn suite_gate_regex() -> String {
    format!(r"(?s)AXTEST_CASE status=pass(?-u:\b).*{AXTEST_SUMMARY_GATE}.*{AXTEST_SUITE_OK}")
}


pub(super) fn require_final_suite_gate(steps: &mut Vec<ShellCheckStep>) {
    if let Some(last) = steps.last_mut()
        && last.shell_cmd.is_none()
        && let Some(patterns) = last.success_regex.as_mut()
        && patterns
            .iter()
            .any(|pattern| pattern.contains(AXTEST_SUITE_OK))
    {
        let gate = suite_gate_regex();
        for pattern in patterns.iter_mut() {
            // The expansion is idempotent: a condition that already carries the
            // gate only needs the other alternatives removed.
            if !pattern.contains(AXTEST_SUMMARY_GATE) {
                *pattern = pattern.replace(AXTEST_SUITE_OK, &gate);
            }
        }
        patterns.retain(|pattern| pattern.contains(AXTEST_SUITE_OK));
        return;
    }
    steps.push(ShellCheckStep {
        success_regex: Some(vec![suite_gate_regex()]),
        ..ShellCheckStep::default()
    });
}


pub(super) fn apply_axtest_board_markers(board: &mut BoardRunConfig) {
    for pattern in [PANIC_FAIL, AXTEST_SUITE_FAIL, AXTEST_CASE_FAIL] {
        ensure_regex(&mut board.fail_regex, pattern);
    }
    require_final_suite_gate(&mut board.shell_check_steps);
}


pub(super) fn apply_ktest_timeout(qemu: &mut QemuConfig, runtime: KtestRuntime, coverage: bool) {
    if qemu.timeout.is_some() {
        return;
    }
    qemu.timeout = Some(match (runtime, coverage) {
        (KtestRuntime::Arceos, false) => 60,
        (KtestRuntime::Arceos, true) => 120,
        (KtestRuntime::Starry | KtestRuntime::Axvisor, false) => 300,
        (KtestRuntime::Starry | KtestRuntime::Axvisor, true) => 360,
        (KtestRuntime::Board, _) => return,
    });
}


pub(super) fn ensure_regex(regexes: &mut Vec<String>, regex: &str) {
    if !regexes.iter().any(|candidate| candidate == regex) {
        regexes.push(regex.to_string());
    }
}

