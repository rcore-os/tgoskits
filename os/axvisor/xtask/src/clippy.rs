// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::{Context, Result, anyhow};
use cargo_metadata::{MetadataCommand, Package};
use colored::*;
use std::collections::HashSet;
use std::process::Command;
use std::time::Instant;

// Import ClippyArgs from main.rs
use super::ClippyArgs;

/// Static target array configuration
const TARGETS: &[&str] = &["x86_64-unknown-none", "aarch64-unknown-none-softfloat"];
const PACKAGE: &str = "axvisor";

/// Clippy check result statistics
#[derive(Debug, Default)]
struct ClippyStats {
    total_checks: usize,
    passed_checks: usize,
    failed_checks: usize,
    skipped_checks: usize,
}

impl ClippyStats {
    fn record_passed(&mut self) {
        self.total_checks += 1;
        self.passed_checks += 1;
    }

    fn record_failed(&mut self) {
        self.total_checks += 1;
        self.failed_checks += 1;
    }

    fn record_skipped(&mut self) {
        self.total_checks += 1;
        self.skipped_checks += 1;
    }

    fn print_summary(&self, duration: std::time::Duration) {
        println!("\n{}", "=== Clippy Check Summary ===".bold().cyan());
        println!("Total checks: {}", self.total_checks);
        println!("Passed checks: {}", self.passed_checks.to_string().green());
        if self.failed_checks > 0 {
            println!(
                "Failed checks: {}",
                self.failed_checks.to_string().red().bold()
            );
        } else {
            println!("Failed checks: {}", self.failed_checks);
        }
        println!(
            "Skipped checks: {}",
            self.skipped_checks.to_string().yellow()
        );
        println!("Execution time: {:.2}s", duration.as_secs_f64());

        if self.failed_checks > 0 {
            println!(
                "\n{}",
                "❌ Clippy errors found, please check the output above"
                    .bold()
                    .red()
            );
        } else {
            println!("\n{}", "✅ All clippy checks passed!".bold().green());
        }
    }
}

/// Main clippy runner function
pub fn run_clippy(args: ClippyArgs) -> Result<()> {
    let start_time = Instant::now();
    println!(
        "{}",
        "🔍 Starting comprehensive Clippy checks...".bold().cyan()
    );

    // Parse package filter, default to axvisor package only
    let package_filter = args
        .packages
        .as_ref()
        .map(|s| {
            s.split(',')
                .map(|s| s.trim().to_string())
                .collect::<HashSet<_>>()
        })
        .or_else(|| Some(HashSet::from([PACKAGE.to_string()])));

    // Parse target filter
    let target_filter = args.targets.as_ref().map(|s| {
        s.split(',')
            .map(|s| s.trim().to_string())
            .collect::<HashSet<_>>()
    });

    // Get workspace metadata
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("Failed to get workspace metadata")?;

    let workspace_members: Vec<&Package> = metadata
        .workspace_members
        .iter()
        .filter_map(|id| metadata.packages.iter().find(|p| p.id == *id))
        .filter(|pkg| {
            // Filter out xtask itself
            pkg.name != "xtask"
        })
        .filter(|pkg| {
            // Apply package filter
            if let Some(ref filter) = package_filter {
                filter.contains(&pkg.name.to_string())
            } else {
                true
            }
        })
        .collect();

    if workspace_members.is_empty() {
        return Err(anyhow!("No matching workspace packages found"));
    }

    println!("Found {} workspace packages:", workspace_members.len());
    for pkg in &workspace_members {
        println!("  - {} ({})", pkg.name.bold(), pkg.version);
    }

    let mut stats = ClippyStats::default();
    let mut has_errors = false;

    // Iterate through all targets
    for target in TARGETS {
        // Apply target filter
        if let Some(ref filter) = target_filter
            && !filter.contains(&target.to_string())
        {
            continue;
        }

        println!(
            "\n{}",
            format!("🎯 Checking target: {target}").bold().yellow()
        );

        // Iterate through all workspace packages
        for package in &workspace_members {
            println!("\n📦 Checking package: {}", package.name.bold());

            // Get all features of the package
            let features = get_package_features(package);

            if features.is_empty() {
                // If no features, perform basic check
                println!("  🔧 No additional features, performing basic check...");

                if args.dry_run {
                    let fix_str = if args.fix { " --fix" } else { "" };
                    let allow_dirty_str = if args.fix && args.allow_dirty {
                        " --allow-dirty"
                    } else {
                        ""
                    };
                    println!(
                        "    [DRY RUN] cargo clippy --target {} -p {}{}{}",
                        target, package.name, fix_str, allow_dirty_str
                    );
                    stats.record_skipped();
                } else {
                    match run_single_clippy(
                        target,
                        package.name.as_ref(),
                        &[],
                        args.continue_on_error,
                        args.fix,
                        args.allow_dirty,
                    ) {
                        Ok(_) => {
                            stats.record_passed();
                        }
                        Err(e) => {
                            stats.record_failed();
                            eprintln!("    {}", format!("❌ Error: {e}").red());
                            if !args.continue_on_error {
                                return Err(e);
                            }
                            has_errors = true;
                        }
                    }
                }
            } else {
                // Iterate through each feature for individual checks
                for feature in &features {
                    println!("  🔧 Checking feature: {}", feature.cyan());

                    if args.dry_run {
                        let fix_str = if args.fix { " --fix" } else { "" };
                        let allow_dirty_str = if args.fix && args.allow_dirty {
                            " --allow-dirty"
                        } else {
                            ""
                        };
                        println!(
                            "    [DRY RUN] cargo clippy --target {} -p {} --features {}{}{}",
                            target, package.name, feature, fix_str, allow_dirty_str
                        );
                        stats.record_skipped();
                    } else {
                        match run_single_clippy(
                            target,
                            package.name.as_ref(),
                            std::slice::from_ref(feature),
                            args.continue_on_error,
                            args.fix,
                            args.allow_dirty,
                        ) {
                            Ok(_) => {
                                stats.record_passed();
                            }
                            Err(e) => {
                                stats.record_failed();
                                eprintln!("    {}", format!("❌ Error: {e}").red());
                                if !args.continue_on_error {
                                    return Err(e);
                                }
                                has_errors = true;
                            }
                        }
                    }
                }

                // Also check all features enabled together
                println!("  🔧 Checking all features together...");
                if args.dry_run {
                    let fix_str = if args.fix { " --fix" } else { "" };
                    let allow_dirty_str = if args.fix && args.allow_dirty {
                        " --allow-dirty"
                    } else {
                        ""
                    };
                    println!(
                        "    [DRY RUN] cargo clippy --target {} -p {} --features {}{}{}",
                        target,
                        package.name,
                        features.join(","),
                        fix_str,
                        allow_dirty_str
                    );
                    stats.record_skipped();
                } else {
                    match run_single_clippy(
                        target,
                        package.name.as_ref(),
                        &features,
                        args.continue_on_error,
                        args.fix,
                        args.allow_dirty,
                    ) {
                        Ok(_) => {
                            stats.record_passed();
                        }
                        Err(e) => {
                            stats.record_failed();
                            eprintln!("    {}", format!("❌ Error: {e}").red());
                            if !args.continue_on_error {
                                return Err(e);
                            }
                            has_errors = true;
                        }
                    }
                }
            }
        }
    }

    let duration = start_time.elapsed();
    stats.print_summary(duration);

    if has_errors {
        Err(anyhow!("Clippy checks found errors"))
    } else {
        Ok(())
    }
}

/// Get all features of a package
fn get_package_features(package: &Package) -> Vec<String> {
    let mut features = Vec::new();

    // Collect all non-empty feature names
    for feature_name in package.features.keys() {
        if !feature_name.is_empty() && !feature_name.starts_with("dep:") {
            features.push(feature_name.clone());
        }
    }

    // Sort for consistent output
    features.sort();
    features
}

/// Run a single clippy check
fn run_single_clippy(
    target: &str,
    package: &str,
    features: &[String],
    continue_on_error: bool,
    fix: bool,
    allow_dirty: bool,
) -> Result<()> {
    let mut args = vec![
        "clippy".to_string(),
        "--target".to_string(),
        target.to_string(),
        "-p".to_string(),
        package.to_string(),
        "--all-targets".to_string(),
    ];

    // Add --fix parameter
    if fix {
        args.push("--fix".to_string());
    }

    // Add --allow-dirty parameter
    if fix && allow_dirty {
        args.push("--allow-dirty".to_string());
    }

    // Add features parameter
    if !features.is_empty() {
        args.push("--features".to_string());
        args.push(features.join(","));
    }

    // Add clippy options
    args.push("--".to_string());
    args.push("-D".to_string()); // Treat all warnings as errors
    args.push("warnings".to_string());

    let mut cmd = Command::new("cargo");
    cmd.args(&args);

    // Set environment variable for stricter checking
    cmd.env("RUSTFLAGS", "-D warnings");

    let fix_str = if fix { " --fix" } else { "" };
    let allow_dirty_str = if fix && allow_dirty {
        " --allow-dirty"
    } else {
        ""
    };
    println!(
        "    Executing: {}",
        format!(
            "cargo clippy --target {} -p {}{}{}{}",
            target,
            package,
            if features.is_empty() {
                String::new()
            } else {
                format!(" --features {}", features.join(","))
            },
            fix_str,
            allow_dirty_str
        )
        .dimmed()
    );

    let output = cmd.output().context(format!(
        "Failed to execute cargo clippy: target={target}, package={package}, features={features:?}"
    ))?;

    if output.status.success() {
        // Even if successful, check if there's output (sometimes clippy has warnings but still returns success)
        if !output.stderr.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("warning:") || stderr.contains("error:") {
                return Err(anyhow!("Clippy output warnings or errors:\n{stderr}"));
            }
        }
        println!("    {}", "✅ Passed".green());
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        let error_msg = if !stderr.is_empty() {
            stderr.to_string()
        } else if !stdout.is_empty() {
            stdout.to_string()
        } else {
            format!(
                "clippy check failed, exit code: {}",
                output.status.code().unwrap_or(-1)
            )
        };

        if continue_on_error {
            eprintln!(
                "    {}",
                format!("⚠️  Error (continuing):\n{error_msg}").yellow()
            );
            Ok(())
        } else {
            Err(anyhow!("Clippy check failed:\n{error_msg}"))
        }
    }
}
