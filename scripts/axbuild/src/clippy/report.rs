use std::{collections::HashMap, path::Path};

use super::check::ClippyCheck;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PackageRunReport {
    pub(super) package: String,
    pub(super) total_checks: usize,
    pub(super) failed_checks: Vec<String>,
}

impl PackageRunReport {
    fn new(package: String) -> Self {
        Self {
            package,
            total_checks: 0,
            failed_checks: Vec::new(),
        }
    }

    fn passed(&self) -> bool {
        self.failed_checks.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClippyRunReport {
    pub(super) total_checks: usize,
    pub(super) passed_checks: usize,
    pub(super) packages: Vec<PackageRunReport>,
}

impl ClippyRunReport {
    pub(super) fn passed_packages(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|package| package.passed())
            .map(|package| package.package.clone())
            .collect()
    }

    pub(super) fn failed_packages(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|package| !package.passed())
            .map(|package| package.package.clone())
            .collect()
    }
}

pub(super) fn planned_clippy_report(checks: &[ClippyCheck]) -> ClippyRunReport {
    let mut packages = Vec::new();
    let mut package_indexes = HashMap::new();

    for check in checks {
        if package_indexes.contains_key(check.package.as_str()) {
            continue;
        }
        let index = packages.len();
        packages.push(PackageRunReport::new(check.package.clone()));
        package_indexes.insert(check.package.clone(), index);
    }

    ClippyRunReport {
        total_checks: checks.len(),
        passed_checks: 0,
        packages,
    }
}

pub(super) fn print_clippy_check_plan(
    workspace_root: &Path,
    index: usize,
    total: usize,
    check: &ClippyCheck,
) {
    let args = check.cargo_args();
    println!("[{}/{}] {}", index + 1, total, check.label());
    if check.env.is_empty() {
        println!(
            "          cd {} && cargo {}",
            workspace_root.display(),
            args.join(" ")
        );
    } else {
        println!(
            "          cd {} && {} cargo {}",
            workspace_root.display(),
            check.env_prefix(),
            args.join(" ")
        );
    }
}

pub(super) fn print_report_summary(report: &ClippyRunReport) {
    println!(
        "clippy summary: {} package(s), {} check(s), {} package(s) passed, {} package(s) failed",
        report.packages.len(),
        report.total_checks,
        report.passed_packages().len(),
        report.failed_packages().len()
    );
    println!(
        "passed checks: {}, failed checks: {}",
        report.passed_checks,
        report.total_checks.saturating_sub(report.passed_checks)
    );

    let failed_packages = report.failed_packages();
    if !failed_packages.is_empty() {
        eprintln!("failed packages: {}", failed_packages.join(", "));
        for package in report.packages.iter().filter(|package| !package.passed()) {
            eprintln!(
                "  {} failed {} check(s): {}",
                package.package,
                package.failed_checks.len(),
                package.failed_checks.join(", ")
            );
        }
    }
}
