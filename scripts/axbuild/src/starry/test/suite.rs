pub(crate) fn finalize_qemu_case_run(report: &StarryQemuRunReport) -> anyhow::Result<()> {
    starry_qemu_summary(report).finish_with_total_detail(
        &starry_qemu_suite_name(report),
        "case",
        Some(&format_duration(report.total_duration)),
    )
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    selected_case: Option<&str>,
    selected_board: Option<&str>,
) -> anyhow::Result<Vec<StarryBoardTestGroup>> {
    let test_suite_dir = require_test_suite_dir(workspace_root)?;
    let groups = collect_board_test_groups(workspace_root, &test_suite_dir)?;
    board_test::filter_board_test_groups(groups, selected_case, selected_board, "Starry", || {
        format!(
            "no Starry board test groups found under {}",
            test_suite_dir.display()
        )
    })
}

pub(crate) fn require_test_suite_dir(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let path = test_suite_root(workspace_root);
    if !path.is_dir() {
        bail!("missing Starry test suite directory `{}`", path.display());
    }
    Ok(path)
}

fn test_suite_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join("test-suit").join(STARRY_TEST_SUITE_OS)
}

pub(crate) fn discover_all_qemu_cases_with_archs(
    workspace_root: &Path,
    selected_case: Option<&str>,
) -> qemu_test::ListQemuCasesResult<Vec<qemu_test::ListedQemuCase>> {
    let test_suite_dir = require_test_suite_dir(workspace_root)?;
    let selection = parse_starry_qemu_case_selection(selected_case);
    let selected_case = if selection.grouped_subcase_filter.is_some() {
        match selection.prefer_direct_case.as_deref() {
            Some(direct_case) if direct_starry_qemu_case_exists(&test_suite_dir, direct_case)? => {
                Some(direct_case)
            }
            _ => selection.parent_case.as_deref(),
        }
    } else {
        selected_case
    };
    qemu_test::discover_all_qemu_cases_with_archs(&test_suite_dir, selected_case, "Starry", "qemu")
}

#[cfg(test)]
pub(crate) fn render_qemu_case_summary(report: &StarryQemuRunReport) -> String {
    starry_qemu_summary(report).render(
        &starry_qemu_suite_name(report),
        "case",
        Some(&format_duration(report.total_duration)),
    )
}

fn starry_qemu_summary(report: &StarryQemuRunReport) -> qemu_test::QemuTestSummary {
    let mut summary = qemu_test::QemuTestSummary::default();
    for case in &report.cases {
        match case.outcome {
            StarryQemuCaseOutcome::Passed => {
                summary.pass_with_detail(&case.name, format_duration(case.duration));
            }
            StarryQemuCaseOutcome::Failed => {
                summary.fail_with_detail(&case.name, format_duration(case.duration));
            }
        }
    }
    summary
}

fn starry_qemu_suite_name(report: &StarryQemuRunReport) -> String {
    let _ = report;
    "starry".to_string()
}

pub(crate) fn format_duration(duration: Duration) -> String {
    format!("{:.2}s", duration.as_secs_f64())
}
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::bail;

use super::{
    STARRY_TEST_SUITE_OS, StarryBoardTestGroup, StarryQemuCaseOutcome, StarryQemuRunReport,
    collect_board_test_groups, direct_starry_qemu_case_exists, parse_starry_qemu_case_selection,
};
use crate::test::{board as board_test, qemu as qemu_test};
