//! Offline agent-review benchmark orchestration.

use std::{fs, path::PathBuf, time::Instant};

use anyhow::{Context, bail};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::context;

mod cases;
mod runner;
mod sandbox;
mod scoring;

use cases::{BenchCase, load_cases, prepare_case, select_cases};
use runner::{AgentKind, AgentOptions, AgentRunner};
use sandbox::ReviewSandbox;
use scoring::{CaseScore, GradeOutput, ReviewOutput, score_review};

const DEFAULT_TIMEOUT_SECS: u64 = 1800;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// List configured benchmark cases without fetching commits
    List,
    /// Validate cases and their historical Git snapshots without invoking an agent CLI
    Check,
    /// Review and grade selected cases sequentially
    Run(RunArgs),
}

#[derive(Args, Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunArgs {
    /// Select a case by exact ID; repeat to select more cases
    #[arg(long = "case", value_name = "ID")]
    cases: Vec<String>,
    /// Select every case sourced from a PR; repeat to select more PRs
    #[arg(long = "pr", value_name = "NUMBER")]
    prs: Vec<u64>,
    /// CLI used for review
    #[arg(long, value_enum, default_value_t = AgentKind::Codex)]
    agent: AgentKind,
    /// Model passed unchanged to the reviewer CLI; omit to inherit its configuration
    #[arg(long)]
    model: Option<String>,
    /// Reasoning effort passed unchanged to the reviewer CLI
    #[arg(long, default_value = "high")]
    reasoning_effort: String,
    /// CLI used for grading; defaults to the reviewer CLI
    #[arg(long, value_enum)]
    grader_agent: Option<AgentKind>,
    /// Model passed unchanged to the grader CLI; defaults to --model
    #[arg(long)]
    grader_model: Option<String>,
    /// Reasoning effort passed unchanged to the grader CLI; defaults to reviewer effort
    #[arg(long)]
    grader_reasoning_effort: Option<String>,
    /// Timeout for each reviewer or grader invocation
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout_secs: u64,
    /// Fail when total recall is below this percentage
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    min_recall: Option<u8>,
    /// Artifact directory; relative paths are resolved from the workspace root
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct RunSummary {
    generated_at: String,
    reviewer: AgentConfiguration,
    grader: AgentConfiguration,
    timeout_secs: u64,
    min_recall: Option<u8>,
    caught: usize,
    expected: usize,
    recall_percent: f64,
    extra_findings: usize,
    cases: Vec<CaseResult>,
}

#[derive(Debug, Serialize)]
struct AgentConfiguration {
    agent: String,
    version: String,
    model: Option<String>,
    reasoning_effort: String,
}

#[derive(Debug, Serialize)]
struct CaseResult {
    case_id: String,
    pr: u64,
    caught: usize,
    expected: usize,
    recall_percent: f64,
    extra_findings: usize,
    review_seconds: f64,
    grade_seconds: f64,
}

pub(crate) async fn execute(command: Command) -> anyhow::Result<()> {
    let workspace_root = context::workspace_root_path()?;
    let all_cases = load_cases(&workspace_root)?;

    match command {
        Command::List => list_cases(&all_cases),
        Command::Check => check_cases(&workspace_root, &all_cases),
        Command::Run(args) => run_cases(&workspace_root, &all_cases, args).await,
    }
}

fn list_cases(cases: &[BenchCase]) -> anyhow::Result<()> {
    for case in cases {
        println!(
            "{}\tPR #{}\t{} expected\t{}",
            case.id,
            case.pr,
            case.expected.len(),
            case.title
        );
    }
    Ok(())
}

fn check_cases(workspace_root: &std::path::Path, cases: &[BenchCase]) -> anyhow::Result<()> {
    for case in cases {
        prepare_case(workspace_root, case)
            .with_context(|| format!("benchmark case `{}` is invalid", case.id))?;
        println!("checked {} (PR #{})", case.id, case.pr);
    }
    println!("OK: {} agent-review benchmark cases", cases.len());
    Ok(())
}

async fn run_cases(
    workspace_root: &std::path::Path,
    all_cases: &[BenchCase],
    args: RunArgs,
) -> anyhow::Result<()> {
    if args.timeout_secs == 0 {
        bail!("--timeout-secs must be greater than zero");
    }
    let grader_kind = args.grader_agent.unwrap_or(args.agent);
    let reviewer = AgentRunner::discover(args.agent)?;
    let grader = AgentRunner::discover(grader_kind)?;
    run_cases_with_runners(workspace_root, all_cases, args, &reviewer, &grader).await
}

async fn run_cases_with_runners(
    workspace_root: &std::path::Path,
    all_cases: &[BenchCase],
    args: RunArgs,
    reviewer: &AgentRunner,
    grader: &AgentRunner,
) -> anyhow::Result<()> {
    if args.timeout_secs == 0 {
        bail!("--timeout-secs must be greater than zero");
    }

    let selected = select_cases(all_cases, &args.cases, &args.prs)?;
    let output_dir = resolve_output_dir(workspace_root, args.output.as_deref());
    fs::create_dir_all(&output_dir).with_context(|| {
        format!(
            "failed to create benchmark artifact directory {}",
            output_dir.display()
        )
    })?;

    let (reviewer_options, grader_options) = resolve_agent_options(&args);
    let reviewer_version = reviewer.version()?;
    let grader_version = grader.version()?;

    let mut results = Vec::with_capacity(selected.len());
    for case in selected {
        println!("reviewing {} (PR #{})", case.id, case.pr);
        prepare_case(workspace_root, case)?;
        let case_dir = output_dir.join(&case.id);
        fs::create_dir_all(&case_dir)?;

        let sandbox = ReviewSandbox::create(workspace_root, case)?;
        let review_path = case_dir.join("review.json");
        let review_started = Instant::now();
        reviewer
            .review(&sandbox, &review_path, &reviewer_options)
            .await
            .with_context(|| format!("reviewer failed for `{}`", case.id))?;
        let review_seconds = review_started.elapsed().as_secs_f64();
        let review = read_json::<ReviewOutput>(&review_path)?;

        let grade_path = case_dir.join("grade.json");
        let grade_started = Instant::now();
        grader
            .grade(case, &review_path, &grade_path, &grader_options)
            .await
            .with_context(|| format!("grader failed for `{}`", case.id))?;
        let grade_seconds = grade_started.elapsed().as_secs_f64();
        let grade = read_json::<GradeOutput>(&grade_path)?;
        let score = score_review(case, &review, &grade)?;
        let result = case_result(case, score, review_seconds, grade_seconds);
        write_json(&case_dir.join("result.json"), &result)?;
        println!(
            "  recall {}/{} ({:.1}%), extra findings {}",
            result.caught, result.expected, result.recall_percent, result.extra_findings
        );
        results.push(result);
    }

    let reviewer_configuration =
        agent_configuration(reviewer, &reviewer_version, &reviewer_options);
    let grader_configuration = agent_configuration(grader, &grader_version, &grader_options);
    let summary = summarize(
        reviewer_configuration,
        grader_configuration,
        args.timeout_secs,
        args.min_recall,
        results,
    );
    write_json(&output_dir.join("summary.json"), &summary)?;
    println!(
        "agent-review recall: {}/{} ({:.1}%); extra findings: {}; artifacts: {}",
        summary.caught,
        summary.expected,
        summary.recall_percent,
        summary.extra_findings,
        output_dir.display()
    );

    if recall_gate_failed(summary.recall_percent, args.min_recall) {
        let min_recall = args
            .min_recall
            .expect("a failed recall gate must have a threshold");
        bail!(
            "agent-review recall {:.1}% is below the requested {}% gate",
            summary.recall_percent,
            min_recall
        );
    }
    Ok(())
}

fn resolve_agent_options(args: &RunArgs) -> (AgentOptions, AgentOptions) {
    let reviewer = AgentOptions {
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        timeout_secs: args.timeout_secs,
    };
    let grader = AgentOptions {
        model: args.grader_model.clone().or_else(|| args.model.clone()),
        reasoning_effort: args
            .grader_reasoning_effort
            .clone()
            .unwrap_or_else(|| args.reasoning_effort.clone()),
        timeout_secs: args.timeout_secs,
    };
    (reviewer, grader)
}

fn resolve_output_dir(
    workspace_root: &std::path::Path,
    output: Option<&std::path::Path>,
) -> PathBuf {
    if let Some(output) = output {
        return if output.is_absolute() {
            output.to_path_buf()
        } else {
            workspace_root.join(output)
        };
    }
    let run_id = Utc::now().format("%Y%m%d-%H%M%S-%3fZ");
    workspace_root
        .join("target/agent-review-bench")
        .join(run_id.to_string())
}

fn case_result(
    case: &BenchCase,
    score: CaseScore,
    review_seconds: f64,
    grade_seconds: f64,
) -> CaseResult {
    CaseResult {
        case_id: case.id.clone(),
        pr: case.pr,
        caught: score.caught,
        expected: score.expected,
        recall_percent: percentage(score.caught, score.expected),
        extra_findings: score.extra_findings,
        review_seconds,
        grade_seconds,
    }
}

fn summarize(
    reviewer: AgentConfiguration,
    grader: AgentConfiguration,
    timeout_secs: u64,
    min_recall: Option<u8>,
    cases: Vec<CaseResult>,
) -> RunSummary {
    let caught = cases.iter().map(|case| case.caught).sum();
    let expected = cases.iter().map(|case| case.expected).sum();
    let extra_findings = cases.iter().map(|case| case.extra_findings).sum();
    RunSummary {
        generated_at: Utc::now().to_rfc3339(),
        reviewer,
        grader,
        timeout_secs,
        min_recall,
        caught,
        expected,
        recall_percent: percentage(caught, expected),
        extra_findings,
        cases,
    }
}

fn agent_configuration(
    runner: &AgentRunner,
    version: &str,
    options: &AgentOptions,
) -> AgentConfiguration {
    AgentConfiguration {
        agent: runner.kind().as_str().to_string(),
        version: version.to_string(),
        model: options.model.clone(),
        reasoning_effort: options.reasoning_effort.clone(),
    }
}

fn recall_gate_failed(recall_percent: f64, min_recall: Option<u8>) -> bool {
    min_recall.is_some_and(|minimum| recall_percent < f64::from(minimum))
}

fn percentage(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        100.0 * numerator as f64 / denominator as f64
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> anyhow::Result<T> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("invalid JSON in {}", path.display()))
}

fn write_json(path: &std::path::Path, value: &impl Serialize) -> anyhow::Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command as ProcessCommand};

    use clap::Parser;

    use super::*;
    #[cfg(unix)]
    use crate::agent_review_bench::cases::{ExpectedFinding, Severity};

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: Command,
    }

    #[test]
    fn parses_run_selectors_and_gate() {
        let cli = TestCli::try_parse_from([
            "bench",
            "run",
            "--case",
            "case-a",
            "--case",
            "case-b",
            "--pr",
            "1495",
            "--min-recall",
            "80",
            "--agent",
            "claude",
            "--model",
            "model with spaces",
            "--reasoning-effort",
            "vendor effort",
            "--grader-agent",
            "codex",
            "--grader-model",
            "grader \"model\"",
            "--grader-reasoning-effort",
            "grader effort",
        ])
        .unwrap();

        let Command::Run(args) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(args.cases, ["case-a", "case-b"]);
        assert_eq!(args.prs, [1495]);
        assert_eq!(args.min_recall, Some(80));
        assert_eq!(args.agent, AgentKind::Claude);
        assert_eq!(args.model.as_deref(), Some("model with spaces"));
        assert_eq!(args.reasoning_effort, "vendor effort");
        assert_eq!(args.grader_agent, Some(AgentKind::Codex));
        assert_eq!(args.grader_model.as_deref(), Some("grader \"model\""));
        assert_eq!(
            args.grader_reasoning_effort.as_deref(),
            Some("grader effort")
        );
    }

    #[test]
    fn parses_default_agent_configuration() {
        let cli = TestCli::try_parse_from(["bench", "run"]).unwrap();
        let Command::Run(args) = cli.command else {
            panic!("expected run command");
        };

        assert_eq!(args.agent, AgentKind::Codex);
        assert_eq!(args.model, None);
        assert_eq!(args.reasoning_effort, "high");
        assert_eq!(args.grader_agent, None);
        assert_eq!(args.grader_model, None);
        assert_eq!(args.grader_reasoning_effort, None);
    }

    #[test]
    fn percentage_handles_empty_and_non_empty_totals() {
        assert_eq!(percentage(0, 0), 0.0);
        assert_eq!(percentage(1, 2), 50.0);
    }

    #[test]
    fn recall_gate_is_opt_in_and_inclusive() {
        assert!(!recall_gate_failed(0.0, None));
        assert!(!recall_gate_failed(80.0, Some(80)));
        assert!(recall_gate_failed(79.9, Some(80)));
    }

    #[test]
    fn grader_options_inherit_reviewer_values() {
        let args = RunArgs {
            cases: Vec::new(),
            prs: Vec::new(),
            agent: AgentKind::Claude,
            model: Some(String::new()),
            reasoning_effort: "effort with spaces".into(),
            grader_agent: None,
            grader_model: None,
            grader_reasoning_effort: None,
            timeout_secs: 7,
            min_recall: None,
            output: None,
        };

        let (reviewer, grader) = resolve_agent_options(&args);
        assert_eq!(reviewer, grader);
        assert_eq!(grader.model.as_deref(), Some(""));
        assert_eq!(grader.reasoning_effort, "effort with spaces");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mock_agents_write_artifacts_and_reject_invalid_json() {
        let workspace = tempfile::tempdir().unwrap();
        let case = create_test_case(workspace.path());
        let output = workspace.path().join("artifacts");
        let program = workspace.path().join("mock-codex");
        write_mock_agent(
            &program,
            r#"{"summary":"found one issue","findings":[{"title":"caught","body":"body","path":"src/lib.rs","line":1,"severity":"major"},{"title":"extra","body":"body","path":"src/lib.rs","line":1,"severity":"minor"}]}"#,
            r#"{"matches":[{"expected_id":"sample-finding","finding_index":0,"reason":"same defect"}]}"#,
        );
        let reviewer = AgentRunner::from_program(AgentKind::Claude, program.clone());
        let grader = AgentRunner::from_program(AgentKind::Codex, program.clone());
        let args = test_run_args(output.clone());

        run_cases_with_runners(workspace.path(), &[case.clone()], args, &reviewer, &grader)
            .await
            .unwrap();

        let case_dir = output.join(&case.id);
        for artifact in [
            case_dir.join("review.json"),
            case_dir.join("grade.json"),
            case_dir.join("result.json"),
            output.join("summary.json"),
        ] {
            assert!(
                artifact.is_file(),
                "missing artifact {}",
                artifact.display()
            );
        }
        let summary = read_json::<serde_json::Value>(&output.join("summary.json")).unwrap();
        assert_eq!(summary["caught"], 1);
        assert_eq!(summary["expected"], 1);
        assert_eq!(summary["extra_findings"], 1);
        assert_eq!(summary["timeout_secs"], 30);
        assert_eq!(summary["reviewer"]["agent"], "claude");
        assert_eq!(summary["reviewer"]["model"], "mock model");
        assert_eq!(summary["reviewer"]["reasoning_effort"], "custom effort");
        assert_eq!(summary["grader"]["agent"], "codex");
        assert_eq!(summary["grader"]["model"], "grader model");
        assert_eq!(summary["grader"]["reasoning_effort"], "grader effort");
        let captured_args = fs::read_to_string(format!("{}.args", program.display())).unwrap();
        assert!(captured_args.contains("/review-single-pr offline-benchmark"));
        assert!(!captured_args.contains("You are performing an offline code review benchmark"));

        let reverse_output = workspace.path().join("reverse-artifacts");
        let mut reverse_args = test_run_args(reverse_output.clone());
        reverse_args.agent = AgentKind::Codex;
        reverse_args.grader_agent = Some(AgentKind::Claude);
        run_cases_with_runners(
            workspace.path(),
            &[case.clone()],
            reverse_args,
            &grader,
            &reviewer,
        )
        .await
        .unwrap();
        let reverse_summary =
            read_json::<serde_json::Value>(&reverse_output.join("summary.json")).unwrap();
        assert_eq!(reverse_summary["reviewer"]["agent"], "codex");
        assert_eq!(reverse_summary["grader"]["agent"], "claude");
        assert_eq!(reverse_summary["caught"], 1);
        let captured_args = fs::read_to_string(format!("{}.args", program.display())).unwrap();
        assert!(captured_args.contains("$review-single-pr offline-benchmark"));

        write_mock_agent(
            &program,
            "not JSON",
            r#"{"matches":[{"expected_id":"sample-finding","finding_index":null,"reason":"missed"}]}"#,
        );
        let invalid_output = workspace.path().join("invalid-artifacts");
        let error = run_cases_with_runners(
            workspace.path(),
            &[case],
            test_run_args(invalid_output),
            &reviewer,
            &grader,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("invalid JSON"), "unexpected error: {error}");
    }

    #[cfg(unix)]
    fn create_test_case(workspace: &Path) -> BenchCase {
        fs::write(workspace.join("AGENTS.md"), "current rules\n").unwrap();
        fs::write(workspace.join("CLAUDE.md"), "see AGENTS.md\n").unwrap();
        fs::create_dir_all(workspace.join("book/guideline")).unwrap();
        fs::write(
            workspace.join("book/guideline/code-quality.md"),
            "current guideline\n",
        )
        .unwrap();
        fs::create_dir_all(workspace.join(".claude/skills/review-single-pr")).unwrap();
        fs::write(
            workspace.join(".claude/skills/review-single-pr/SKILL.md"),
            "current review skill\n",
        )
        .unwrap();
        git(workspace, &["init", "--quiet"]);
        git(workspace, &["config", "user.name", "test"]);
        git(workspace, &["config", "user.email", "test@invalid"]);
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();
        git(workspace, &["add", "--all"]);
        git(workspace, &["commit", "--quiet", "-m", "base"]);
        let base = git_output(workspace, &["rev-parse", "HEAD"]);

        fs::write(workspace.join("src/lib.rs"), "pub fn value() -> u8 { 2 }\n").unwrap();
        git(workspace, &["add", "--all"]);
        git(workspace, &["commit", "--quiet", "-m", "head"]);
        let head = git_output(workspace, &["rev-parse", "HEAD"]);

        fs::write(workspace.join("fixed.txt"), "fixed\n").unwrap();
        git(workspace, &["add", "--all"]);
        git(workspace, &["commit", "--quiet", "-m", "fix"]);
        let fixed_by = git_output(workspace, &["rev-parse", "HEAD"]);

        BenchCase {
            id: "0001-sample".into(),
            pr: 1,
            title: "sample change".into(),
            remote: "https://example.invalid/repo.git".into(),
            base,
            head,
            source: "secret source".into(),
            fixed_by,
            expected: vec![ExpectedFinding {
                id: "sample-finding".into(),
                path: "src/lib.rs".into(),
                line: 1,
                severity: Severity::Major,
                description: "secret answer".into(),
                match_if: "same defect".into(),
            }],
        }
    }

    #[cfg(unix)]
    fn test_run_args(output: PathBuf) -> RunArgs {
        RunArgs {
            cases: Vec::new(),
            prs: Vec::new(),
            agent: AgentKind::Claude,
            model: Some("mock model".into()),
            reasoning_effort: "custom effort".into(),
            grader_agent: Some(AgentKind::Codex),
            grader_model: Some("grader model".into()),
            grader_reasoning_effort: Some("grader effort".into()),
            timeout_secs: 30,
            min_recall: None,
            output: Some(output),
        }
    }

    #[cfg(unix)]
    fn write_mock_agent(program: &Path, review: &str, grade: &str) {
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli mock'; exit 0; \
             fi\nprintf '%s\\n' \"$@\" >> \"$0.args\"\nmode=review\nif [ -f expected.json ]; then \
             mode=grade; fi\noutput=\nwhile [ \"$#\" -gt 0 ]; do\ncase \"$1\" in\n--cd) shift; \
             mode=grade ;;\n--output-last-message|-o) shift; output=$1 ;;\nesac\nshift\ndone\nif \
             [ \"$mode\" = review ]; then payload='{review}'; else payload='{grade}'; fi\nif [ -n \
             \"$output\" ]; then printf '%s\\n' \"$payload\" > \"$output\"; else printf '%s\\n' \
             \"$payload\"; fi\n"
        );
        fs::write(program, script).unwrap();
        fs::set_permissions(program, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn git(workspace: &Path, args: &[&str]) {
        assert!(
            ProcessCommand::new("git")
                .current_dir(workspace)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    #[cfg(unix)]
    fn git_output(workspace: &Path, args: &[&str]) -> String {
        let output = ProcessCommand::new("git")
            .current_dir(workspace)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().into()
    }
}
