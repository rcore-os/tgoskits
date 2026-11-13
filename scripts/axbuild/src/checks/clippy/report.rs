use std::{collections::HashMap, path::Path};

use super::check::ClippyCheck;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PackageRunReport {
    pub(super) package: String,
    pub(super) planned_checks: usize,
    pub(super) total_checks: usize,
    pub(super) failed_checks: Vec<String>,
}

impl PackageRunReport {
    fn new(package: String) -> Self {
        Self {
            package,
            planned_checks: 0,
            total_checks: 0,
            failed_checks: Vec::new(),
        }
    }

    fn passed(&self) -> bool {
        self.total_checks == self.planned_checks && self.failed_checks.is_empty()
    }

    fn failed(&self) -> bool {
        !self.failed_checks.is_empty()
    }

    fn skipped(&self) -> bool {
        !self.passed() && !self.failed()
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
            .filter(|package| package.failed())
            .map(|package| package.package.clone())
            .collect()
    }

    pub(super) fn skipped_packages(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|package| package.skipped())
            .map(|package| package.package.clone())
            .collect()
    }
}

pub(super) fn planned_clippy_report(checks: &[ClippyCheck]) -> ClippyRunReport {
    let mut packages: Vec<PackageRunReport> = Vec::new();
    let mut package_indexes: HashMap<String, usize> = HashMap::new();

    for check in checks {
        if let Some(&index) = package_indexes.get(check.package.as_str()) {
            packages[index].planned_checks += 1;
            continue;
        }
        let index = packages.len();
        let mut package = PackageRunReport::new(check.package.clone());
        package.planned_checks = 1;
        packages.push(package);
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
    let invocation = check.cargo_invocation();
    println!("[{}/{}] {}", index + 1, total, check.label());
    if invocation.env.is_empty() {
        println!(
            "          cd {} && cargo {}",
            workspace_root.display(),
            invocation.args.join(" ")
        );
    } else {
        println!(
            "          cd {} && {} cargo {}",
            workspace_root.display(),
            check.env_prefix(),
            invocation.args.join(" ")
        );
    }
}

pub(super) fn print_report_summary(report: &ClippyRunReport) {
    println!(
        "clippy summary: {} package(s), {} check(s), {} package(s) passed, {} package(s) failed, \
         {} package(s) skipped",
        report.packages.len(),
        report.total_checks,
        report.passed_packages().len(),
        report.failed_packages().len(),
        report.skipped_packages().len()
    );
    println!(
        "passed checks: {}, failed checks: {}, skipped checks: {}",
        report.passed_checks,
        report
            .packages
            .iter()
            .map(|package| package.failed_checks.len())
            .sum::<usize>(),
        report.total_checks.saturating_sub(
            report
                .packages
                .iter()
                .map(|package| package.total_checks)
                .sum::<usize>()
        )
    );

    let failed_packages = report.failed_packages();
    if !failed_packages.is_empty() {
        eprintln!("failed packages: {}", failed_packages.join(", "));
        for package in report.packages.iter().filter(|package| package.failed()) {
            eprintln!(
                "  {} failed {} check(s): {}",
                package.package,
                package.failed_checks.len(),
                package.failed_checks.join(", ")
            );
        }
    }
    let skipped_packages = report.skipped_packages();
    if !skipped_packages.is_empty() {
        eprintln!("skipped packages: {}", skipped_packages.join(", "));
    }
}
