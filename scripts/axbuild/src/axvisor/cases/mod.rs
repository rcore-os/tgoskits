use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use colored::Colorize;
use serde::Serialize;

use crate::{
    axvisor::{cli::ArgsTestCases, context::AxvisorContext},
    context::AppContext,
};

pub(crate) mod build;
pub(crate) mod execute;
pub(crate) mod manifest;
pub(crate) mod report;
pub(crate) mod rootfs;
pub(crate) mod session;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Selection {
    Suite(PathBuf),
    Case(PathBuf),
}

#[derive(Debug, Clone)]
pub(super) struct RunArtifacts {
    pub(super) run_id: String,
    pub(super) run_dir: PathBuf,
    pub(super) target_rootfs: PathBuf,
    pub(super) summary_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct CasePlan {
    pub(super) arch: String,
    pub(super) guest_log: bool,
    pub(super) selection: Selection,
    pub(super) suite_name: Option<String>,
    pub(super) cases: Vec<manifest::LoadedCase>,
}

pub(crate) async fn run(
    args: ArgsTestCases,
    app: &mut AppContext,
    ctx: &AxvisorContext,
) -> anyhow::Result<()> {
    let plan = CasePlan::from_args(args, ctx.workspace_root())?;
    print_section("AxVisor Cases");
    print_plan_overview(&plan);

    print_phase(1, 5, "Resolve case layouts");
    let layouts = build::resolve_case_layouts(&plan.cases, &plan.arch)?;

    print_phase(2, 5, "Prepare run artifacts");
    let artifacts = rootfs::prepare_run_artifacts(ctx, &plan.arch).await?;

    print_phase(3, 5, "Build and stage guest cases");
    let prepared_cases =
        build::build_and_stage_cases(app, ctx, &plan.cases, &layouts, &artifacts, &plan.arch)
            .await?;

    print_phase(4, 5, "Execute guest cases on AxVisor");
    let execution = execute::run(&plan, app, ctx, &artifacts, &prepared_cases).await?;

    print_phase(5, 5, "Write summary");
    report::write_summary(
        &artifacts.summary_path,
        &plan,
        &artifacts,
        &prepared_cases,
        &execution,
    )?;

    print_section("Cases");
    for (index, case) in plan.cases.iter().enumerate() {
        let case_index = format!("[{}]", index + 1).bold().blue();
        println!(
            "{} {} timeout={}s",
            case_index, case.manifest.id, case.manifest.timeout_secs
        );
        println!("    dir           : {}", case.case_dir.display());
    }

    print_section("Artifacts");
    print_kv("run_id", &artifacts.run_id);
    print_kv("run_dir", artifacts.run_dir.display());
    print_kv("target_rootfs", artifacts.target_rootfs.display());
    print_kv("summary", artifacts.summary_path.display());
    print_kv("axvisor_build", &execution.axvisor_build_config);
    print_kv("host_log", &execution.axvisor_host_log);
    for (layout, prepared) in layouts.iter().zip(&prepared_cases) {
        println!("- case {}", prepared.case_id);
        print_kv("  package", &prepared.package);
        print_kv("  target", &prepared.target);
        print_kv("  asset_key", &prepared.asset_key);
        print_kv("  build_config", prepared.build_info_path.display());
        print_kv("  runtime", prepared.runtime_artifact_path.display());
        print_kv("  vm_template", layout.vm_template.display());
        print_kv(
            "  staged_kernel",
            prepared.staged_kernel_host_path.display(),
        );
        print_kv("  staged_vm", prepared.rendered_vm_host_path.display());
        print_kv("  guest_kernel", &prepared.guest_kernel_path);
        print_kv("  guest_vm", &prepared.guest_vm_config_path);
    }

    let passed = execution
        .cases
        .iter()
        .filter(|record| record.outcome.is_success())
        .count();
    let total = execution.cases.len();

    print_section("Results");
    let summary = if passed == total {
        format!("passed {passed}/{total} case(s)").bold().green()
    } else {
        format!("passed {passed}/{total} case(s)").bold().yellow()
    };
    println!("{summary}");
    for record in &execution.cases {
        println!(
            "- {} {}: {}",
            record.outcome.label(),
            record.id,
            record.detail
        );
    }

    if execution.passed {
        Ok(())
    } else {
        bail!("axvisor case run failed; see summary for details")
    }
}

impl CasePlan {
    fn from_args(args: ArgsTestCases, workspace_root: &Path) -> anyhow::Result<Self> {
        let arch = args.arch;
        let selection = if let Some(path) = args.suite {
            Selection::Suite(resolve_cli_path(workspace_root, &path))
        } else if let Some(path) = args.case {
            Selection::Case(resolve_cli_path(workspace_root, &path))
        } else {
            unreachable!("clap ensures either --suite or --case is present");
        };

        let guest_log = args
            .guest_log
            .unwrap_or(matches!(selection, Selection::Case(_)));

        let (suite_name, cases) = match &selection {
            Selection::Suite(path) => {
                let (suite, cases) = manifest::load_cases_from_suite(workspace_root, path, &arch)
                    .with_context(|| {
                    format!("failed to load case suite manifest {}", path.display())
                })?;
                (Some(suite.name), cases)
            }
            Selection::Case(path) => (None, {
                let case = manifest::load_case_from_dir(path)
                    .with_context(|| format!("failed to load case from {}", path.display()))?;
                manifest::ensure_case_supports_arch(&case, &arch)?;
                vec![case]
            }),
        };

        Ok(Self {
            arch,
            guest_log,
            selection,
            suite_name,
            cases,
        })
    }

    fn to_summary<'a>(&'a self, artifacts: &'a RunArtifacts) -> SummaryRecord<'a> {
        SummaryRecord {
            run_id: &artifacts.run_id,
            arch: &self.arch,
            guest_log: self.guest_log,
            run_dir: artifacts.run_dir.display().to_string(),
            target_rootfs: artifacts.target_rootfs.display().to_string(),
            selection: SummarySelectionRecord {
                kind: self.selection.kind(),
                path: self.selection.path().display().to_string(),
                suite_name: self.suite_name.as_deref(),
            },
            cases: self
                .cases
                .iter()
                .map(|case| SummaryCaseRecord {
                    id: &case.manifest.id,
                    case_dir: case.case_dir.display().to_string(),
                    timeout_secs: case.manifest.timeout_secs,
                })
                .collect(),
        }
    }
}

fn resolve_cli_path(workspace_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

fn print_section(title: &str) {
    println!();
    println!("{}", format!("== {title} ==").bold().cyan());
}

fn print_phase(index: usize, total: usize, title: &str) {
    let step = format!("[{index}/{total}]").bold().blue();
    println!("{step} {}", title.bold());
}

fn print_kv(label: &str, value: impl std::fmt::Display) {
    println!("{:<14}: {}", label.bold(), value);
}

fn print_plan_overview(plan: &CasePlan) {
    match &plan.selection {
        Selection::Suite(path) => {
            print_kv("selection", "suite");
            print_kv("suite", plan.suite_name.as_deref().unwrap_or("<unnamed>"));
            print_kv("manifest", path.display());
        }
        Selection::Case(path) => {
            print_kv("selection", "case");
            print_kv("case_dir", path.display());
        }
    }
    print_kv("arch", &plan.arch);
    print_kv("guest_log", plan.guest_log);
    print_kv("cases", plan.cases.len());
}

impl Selection {
    fn kind(&self) -> &'static str {
        match self {
            Self::Suite(_) => "suite",
            Self::Case(_) => "case",
        }
    }

    fn path(&self) -> &Path {
        match self {
            Self::Suite(path) | Self::Case(path) => path.as_path(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SummaryCaseRecord<'a> {
    id: &'a str,
    case_dir: String,
    timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
struct SummarySelectionRecord<'a> {
    kind: &'a str,
    path: String,
    suite_name: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
struct SummaryRecord<'a> {
    run_id: &'a str,
    arch: &'a str,
    guest_log: bool,
    run_dir: String,
    target_rootfs: String,
    selection: SummarySelectionRecord<'a>,
    cases: Vec<SummaryCaseRecord<'a>>,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn case_plan_defaults_guest_log_true_for_single_case() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path();
        let case_dir = workspace_root.join("test-suit/axvisor/example/pass-report");
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(
            case_dir.join("case.toml"),
            r#"
id = "example.pass"
arch = ["aarch64"]
timeout_secs = 5
"#,
        )
        .unwrap();

        let args = ArgsTestCases {
            arch: "aarch64".to_string(),
            suite: None,
            case: Some(PathBuf::from("test-suit/axvisor/example/pass-report")),
            guest_log: None,
        };

        let plan = CasePlan::from_args(args, workspace_root).unwrap();
        assert!(plan.guest_log);
        assert_eq!(plan.cases.len(), 1);
    }

    #[test]
    fn case_plan_defaults_guest_log_false_for_suite() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path();
        let case_dir = workspace_root.join("test-suit/axvisor/example/pass-report");
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(
            case_dir.join("case.toml"),
            r#"
id = "example.pass"
arch = ["aarch64"]
timeout_secs = 5
"#,
        )
        .unwrap();

        let suite_dir = workspace_root.join("test-suit/axvisor/suites");
        fs::create_dir_all(&suite_dir).unwrap();
        fs::write(
            suite_dir.join("examples.toml"),
            r#"
name = "examples"

[arches.aarch64]
cases = ["example/pass-report"]
"#,
        )
        .unwrap();

        let args = ArgsTestCases {
            arch: "aarch64".to_string(),
            suite: Some(PathBuf::from("test-suit/axvisor/suites/examples.toml")),
            case: None,
            guest_log: None,
        };

        let plan = CasePlan::from_args(args, workspace_root).unwrap();
        assert!(!plan.guest_log);
        assert_eq!(plan.suite_name.as_deref(), Some("examples"));
        assert_eq!(plan.cases.len(), 1);
    }

    #[test]
    fn case_plan_summary_contains_selection_and_case_metadata() {
        let plan = CasePlan {
            arch: "aarch64".to_string(),
            guest_log: false,
            selection: Selection::Case(PathBuf::from("/tmp/case")),
            suite_name: None,
            cases: vec![manifest::LoadedCase {
                case_dir: PathBuf::from("/tmp/case"),
                manifest: manifest::CaseManifest {
                    id: "example.pass".to_string(),
                    tags: vec!["example".to_string(), "runner".to_string()],
                    arch: vec!["aarch64".to_string()],
                    timeout_secs: 7,
                    description: None,
                },
            }],
        };
        let artifacts = RunArtifacts {
            run_id: "run-1".to_string(),
            run_dir: PathBuf::from("/tmp/run-1"),
            target_rootfs: PathBuf::from("/tmp/run-1/rootfs.img"),
            summary_path: PathBuf::from("/tmp/run-1/summary.json"),
        };

        let summary = serde_json::to_value(plan.to_summary(&artifacts)).unwrap();
        assert_eq!(summary["run_id"], Value::String("run-1".to_string()));
        assert_eq!(
            summary["selection"]["kind"],
            Value::String("case".to_string())
        );
        assert_eq!(
            summary["cases"][0]["id"],
            Value::String("example.pass".to_string())
        );
        assert_eq!(summary["cases"][0]["timeout_secs"], Value::from(7_u64));
    }
}
