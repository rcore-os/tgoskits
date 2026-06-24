use super::*;

pub(crate) fn finalize_qemu_test_run(
    suite_name: &str,
    unit: &str,
    failed: &[String],
) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all {} qemu tests passed", suite_name);
        Ok(())
    } else {
        bail!(
            "{} qemu tests failed for {} {}(s): {}",
            suite_name,
            failed.len(),
            unit,
            failed.join(", ")
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QemuTestOutcome {
    Passed,
    Failed,
}

#[derive(Debug)]
pub(super) struct QemuTestSummaryEntry {
    name: String,
    outcome: QemuTestOutcome,
    detail: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct QemuTestSummary {
    entries: Vec<QemuTestSummaryEntry>,
}

impl QemuTestSummary {
    pub(crate) fn pass_with_detail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.record(QemuTestOutcome::Passed, name, Some(detail.into()));
    }

    pub(crate) fn fail_with_detail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.record(QemuTestOutcome::Failed, name, Some(detail.into()));
    }

    pub(crate) fn finish_with_total_detail(
        &self,
        suite_name: &str,
        unit: &str,
        total_detail: Option<&str>,
    ) -> anyhow::Result<()> {
        println!();
        println!("{}", self.render(suite_name, unit, total_detail));

        let failed = self.failed_names();
        finalize_qemu_test_run(suite_name, unit, &failed)
    }

    pub(crate) fn render(
        &self,
        suite_name: &str,
        unit: &str,
        total_detail: Option<&str>,
    ) -> String {
        let mut lines = Vec::new();
        lines.push(format!("{suite_name} qemu test summary:"));

        for entry in &self.entries {
            let status = match entry.outcome {
                QemuTestOutcome::Passed => "PASS",
                QemuTestOutcome::Failed => "FAIL",
            };
            if let Some(detail) = entry.detail.as_ref() {
                lines.push(format!("  {status} {} ({detail})", entry.name));
            } else {
                lines.push(format!("  {status} {}", entry.name));
            }
        }

        let passed = self.passed_count();
        let total = self.entries.len();
        lines.push(format!("result: {passed}/{total} {unit}(s) passed"));
        if let Some(total_detail) = total_detail {
            lines.push(format!("total: {total_detail}"));
        }

        lines.join("\n")
    }

    fn record(
        &mut self,
        outcome: QemuTestOutcome,
        name: impl Into<String>,
        detail: Option<String>,
    ) {
        self.entries.push(QemuTestSummaryEntry {
            name: name.into(),
            outcome,
            detail,
        });
    }

    fn passed_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.outcome == QemuTestOutcome::Passed)
            .count()
    }

    fn failed_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.outcome == QemuTestOutcome::Failed)
            .map(|entry| entry.name.clone())
            .collect()
    }
}
