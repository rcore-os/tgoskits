use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, bail};
use clap::ValueEnum;
use tempfile::tempdir;
use tokio::{process::Command, time::timeout};

use super::{cases::BenchCase, sandbox::ReviewSandbox, scoring::ReviewFinding};

const CODEX_REVIEW_PROMPT: &str = "$review-single-pr offline-benchmark";
const CLAUDE_REVIEW_PROMPT: &str = "/review-single-pr offline-benchmark";
const GRADE_PROMPT: &str =
    "You are grading one offline code-review case. Read only `known_findings.json` and \
     `candidate_findings.json` in the current directory. For every known finding, decide whether \
     one or more candidate findings identify the same underlying defect or material risk. \
     Different wording, anchors, omitted consequences, and different or absent remediation are \
     acceptable; exact numbers, PR references, and historical context are not required. Candidate \
     findings may jointly cover one known finding. Generic advice, mere proximity, or a different \
     issue does not match. Return exactly one match object per known finding ID. \
     `finding_indices` are zero-based indices in `candidate_findings.json` and must contain every \
     candidate used to support the match, or be empty when missed. Do not inspect any other paths.";
const GRADE_SCHEMA: &str = include_str!("../../../agent-review-bench/schemas/grade.schema.json");

const CLAUDE_COMMON_ARGS: &[&str] = &[
    "-p",
    "--no-session-persistence",
    "--strict-mcp-config",
    "--mcp-config",
    r#"{"mcpServers":{}}"#,
    "--no-chrome",
    "--permission-mode",
    "dontAsk",
];
const CLAUDE_REVIEW_SETTING_SOURCES: &str = "user,project";
const CLAUDE_REVIEW_SETTINGS: &str = r#"{
    "disableAllHooks": true,
    "disableAgentView": true,
    "disableSkillShellExecution": true,
    "env": {
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1",
        "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS": "1",
        "CLAUDE_CODE_DISABLE_CLAUDE_MDS": "1",
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL": "1"
    }
}"#;
const CLAUDE_GRADE_ARGS: &[&str] = &["--safe-mode"];
const CLAUDE_REVIEW_TOOLS: &str = "Bash,Read,Glob,Grep";
const CLAUDE_REVIEW_ALLOWED_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "Bash(git diff *)",
    "Bash(git show *)",
    "Bash(git status *)",
    "Bash(git log *)",
    "Bash(git grep *)",
];
const CLAUDE_GRADE_TOOLS: &str = "Read";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(super) enum AgentKind {
    #[default]
    Codex,
    Claude,
}

impl AgentKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    fn program(self) -> &'static str {
        self.as_str()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentOptions {
    pub(super) model: Option<String>,
    pub(super) reasoning_effort: String,
    pub(super) timeout_secs: u64,
}

pub(super) struct AgentRunner {
    kind: AgentKind,
    program: PathBuf,
}

impl AgentRunner {
    pub(super) fn discover(kind: AgentKind) -> anyhow::Result<Self> {
        let runner = Self {
            kind,
            program: PathBuf::from(kind.program()),
        };
        runner
            .version()
            .with_context(|| format!("{} CLI is unavailable", kind.as_str()))?;
        Ok(runner)
    }

    pub(super) fn kind(&self) -> AgentKind {
        self.kind
    }

    pub(super) fn version(&self) -> anyhow::Result<String> {
        let output = std::process::Command::new(&self.program)
            .arg("--version")
            .output()
            .with_context(|| format!("failed to execute {} --version", self.program.display()))?;
        if !output.status.success() {
            bail!(
                "{} --version exited with {}",
                self.program.display(),
                output.status
            );
        }
        let version = String::from_utf8(output.stdout).context("agent version was not UTF-8")?;
        let version = version.trim();
        if version.is_empty() {
            bail!("{} CLI returned an empty version", self.kind.as_str());
        }
        Ok(version.to_string())
    }

    pub(super) async fn review(
        &self,
        sandbox: &ReviewSandbox,
        artifact_path: &Path,
        options: &AgentOptions,
    ) -> anyhow::Result<()> {
        match self.kind {
            AgentKind::Codex => {
                self.review_with_codex(sandbox, artifact_path, options)
                    .await
            }
            AgentKind::Claude => {
                self.review_with_claude(sandbox, artifact_path, options)
                    .await
            }
        }
    }

    pub(super) async fn grade(
        &self,
        case: &BenchCase,
        findings: &[ReviewFinding],
        artifact_path: &Path,
        options: &AgentOptions,
    ) -> anyhow::Result<()> {
        let grader_dir = tempdir().context("failed to create isolated grader directory")?;
        let known_findings = serde_json::to_string_pretty(&case.expected)?;
        fs::write(
            grader_dir.path().join("known_findings.json"),
            known_findings,
        )?;
        let candidate_findings = serde_json::to_string_pretty(findings)?;
        fs::write(
            grader_dir.path().join("candidate_findings.json"),
            candidate_findings,
        )?;
        let schema_path = grader_dir.path().join("grade.schema.json");
        fs::write(&schema_path, GRADE_SCHEMA)?;
        let temporary_output = grader_dir.path().join("grade.json");

        match self.kind {
            AgentKind::Codex => {
                self.grade_with_codex(grader_dir.path(), &schema_path, &temporary_output, options)
                    .await?;
            }
            AgentKind::Claude => {
                self.grade_with_claude(grader_dir.path(), &temporary_output, options)
                    .await?;
            }
        }
        copy_nonempty_output(&temporary_output, artifact_path, "grader")
    }

    async fn review_with_codex(
        &self,
        sandbox: &ReviewSandbox,
        artifact_path: &Path,
        options: &AgentOptions,
    ) -> anyhow::Result<()> {
        let temporary_output = sandbox.temporary_review_output();
        remove_if_exists(&temporary_output)?;
        let mut command = Command::new(&self.program);
        command
            .current_dir(sandbox.repo())
            .args(["exec", "--ephemeral", "--sandbox", "read-only"])
            .args(["-c", "approval_policy=\"never\""])
            .args(["-c", "sandbox_workspace_write.network_access=false"])
            .arg("--output-schema")
            .arg(sandbox.review_schema())
            .arg("--output-last-message")
            .arg(&temporary_output)
            .stdout(Stdio::inherit());
        apply_codex_options(&mut command, options);
        command.arg(CODEX_REVIEW_PROMPT);

        run_process(&mut command, options.timeout_secs, "Codex reviewer").await?;
        copy_nonempty_output(&temporary_output, artifact_path, "reviewer")
    }

    async fn review_with_claude(
        &self,
        sandbox: &ReviewSandbox,
        artifact_path: &Path,
        options: &AgentOptions,
    ) -> anyhow::Result<()> {
        let temporary_output = sandbox.temporary_review_output();
        remove_if_exists(&temporary_output)?;
        let output = fs::File::create(&temporary_output)?;
        let schema = claude_schema(&fs::read_to_string(sandbox.review_schema())?)?;
        let mut command = Command::new(&self.program);
        command
            .current_dir(sandbox.repo())
            .args(CLAUDE_COMMON_ARGS)
            .args(["--setting-sources", CLAUDE_REVIEW_SETTING_SOURCES])
            .args(["--settings", CLAUDE_REVIEW_SETTINGS])
            .args(["--tools", CLAUDE_REVIEW_TOOLS])
            .arg("--allowedTools")
            .args(CLAUDE_REVIEW_ALLOWED_TOOLS)
            .args(["--json-schema", &schema])
            .stdout(Stdio::from(output));
        apply_claude_options(&mut command, options);
        command.arg(CLAUDE_REVIEW_PROMPT);

        run_process(&mut command, options.timeout_secs, "Claude reviewer").await?;
        copy_nonempty_output(&temporary_output, artifact_path, "reviewer")
    }

    async fn grade_with_codex(
        &self,
        grader_dir: &Path,
        schema_path: &Path,
        temporary_output: &Path,
        options: &AgentOptions,
    ) -> anyhow::Result<()> {
        let mut command = Command::new(&self.program);
        command
            .args(["exec", "--skip-git-repo-check", "--ephemeral"])
            .args(["--sandbox", "read-only"])
            .args(["-c", "approval_policy=\"never\""])
            .args(["-c", "sandbox_workspace_write.network_access=false"])
            .arg("--cd")
            .arg(grader_dir)
            .arg("--output-schema")
            .arg(schema_path)
            .arg("--output-last-message")
            .arg(temporary_output)
            .stdout(Stdio::inherit());
        apply_codex_options(&mut command, options);
        command.arg(GRADE_PROMPT);

        run_process(&mut command, options.timeout_secs, "Codex grader").await
    }

    async fn grade_with_claude(
        &self,
        grader_dir: &Path,
        temporary_output: &Path,
        options: &AgentOptions,
    ) -> anyhow::Result<()> {
        let output = fs::File::create(temporary_output)?;
        let schema = claude_schema(GRADE_SCHEMA)?;
        let mut command = Command::new(&self.program);
        command
            .current_dir(grader_dir)
            .args(CLAUDE_COMMON_ARGS)
            .args(CLAUDE_GRADE_ARGS)
            .args(["--tools", CLAUDE_GRADE_TOOLS])
            .args(["--allowedTools", "Read"])
            .args(["--json-schema", &schema])
            .stdout(Stdio::from(output));
        apply_claude_options(&mut command, options);
        command.arg(GRADE_PROMPT);

        run_process(&mut command, options.timeout_secs, "Claude grader").await
    }

    #[cfg(test)]
    pub(super) fn from_program(kind: AgentKind, program: PathBuf) -> Self {
        Self { kind, program }
    }
}

fn apply_codex_options(command: &mut Command, options: &AgentOptions) {
    if let Some(model) = &options.model {
        command.args(["--model", model]);
    }
    let effort = toml::Value::String(options.reasoning_effort.clone()).to_string();
    command.args(["-c", &format!("model_reasoning_effort={effort}")]);
}

fn apply_claude_options(command: &mut Command, options: &AgentOptions) {
    if let Some(model) = &options.model {
        command.args(["--model", model]);
    }
    command.args(["--effort", &options.reasoning_effort]);
}

fn claude_schema(schema: &str) -> anyhow::Result<String> {
    let mut schema = serde_json::from_str::<serde_json::Value>(schema)
        .context("invalid JSON Schema supplied to Claude")?;
    let object = schema
        .as_object_mut()
        .context("Claude JSON Schema must be an object")?;
    object.remove("$schema");
    serde_json::to_string(&schema).context("failed to serialize Claude JSON Schema")
}

async fn run_process(
    command: &mut Command,
    timeout_secs: u64,
    description: &str,
) -> anyhow::Result<()> {
    command
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {description}"))?;
    let status = match timeout(Duration::from_secs(timeout_secs), child.wait()).await {
        Ok(status) => status.with_context(|| format!("failed to wait for {description}"))?,
        Err(_) => {
            child
                .kill()
                .await
                .with_context(|| format!("failed to terminate timed-out {description}"))?;
            let _ = child.wait().await;
            bail!("{description} timed out after {timeout_secs} seconds");
        }
    };
    if status.success() {
        Ok(())
    } else {
        bail!("{description} exited with status {status}")
    }
}

fn copy_nonempty_output(source: &Path, destination: &Path, role: &str) -> anyhow::Result<()> {
    let metadata = fs::metadata(source)
        .with_context(|| format!("{role} did not create {}", source.display()))?;
    if metadata.len() == 0 {
        bail!("{role} created an empty output file");
    }
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to copy {role} output from {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{ffi::OsStr, fs, os::unix::fs::PermissionsExt};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn reads_version_from_injected_agent_program() {
        let temp = tempdir().unwrap();
        let program = temp.path().join("mock-agent");
        fs::write(&program, "#!/bin/sh\necho 'agent test-version'\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

        let runner = AgentRunner::from_program(AgentKind::Claude, program);
        assert_eq!(runner.version().unwrap(), "agent test-version");
    }

    #[test]
    fn model_and_effort_are_passed_as_single_argv_values() {
        let options = AgentOptions {
            model: Some("model with \"quotes\"; $(touch ignored)".into()),
            reasoning_effort: "vendor effort 'x' $HOME\\next".into(),
            timeout_secs: 1,
        };

        let mut codex = Command::new("codex");
        apply_codex_options(&mut codex, &options);
        let codex_args = args(&codex);
        assert_eq!(
            codex_args[0..2],
            ["--model", "model with \"quotes\"; $(touch ignored)"]
        );
        assert_eq!(codex_args[2], "-c");
        let config = codex_args[3]
            .strip_prefix("model_reasoning_effort=")
            .unwrap();
        assert_eq!(
            config.parse::<toml::Value>().unwrap(),
            toml::Value::String("vendor effort 'x' $HOME\\next".into())
        );

        let mut claude = Command::new("claude");
        apply_claude_options(&mut claude, &options);
        assert_eq!(
            args(&claude),
            [
                "--model",
                "model with \"quotes\"; $(touch ignored)",
                "--effort",
                "vendor effort 'x' $HOME\\next"
            ]
        );

        let empty = AgentOptions {
            model: Some(String::new()),
            reasoning_effort: String::new(),
            timeout_secs: 1,
        };
        let mut claude = Command::new("claude");
        apply_claude_options(&mut claude, &empty);
        assert_eq!(args(&claude), ["--model", "", "--effort", ""]);

        let omitted = AgentOptions {
            model: None,
            reasoning_effort: "high".into(),
            timeout_secs: 1,
        };
        let mut claude = Command::new("claude");
        apply_claude_options(&mut claude, &omitted);
        assert_eq!(args(&claude), ["--effort", "high"]);
    }

    #[test]
    fn claude_tool_sets_are_read_only_and_offline() {
        assert_eq!(CLAUDE_GRADE_TOOLS, "Read");
        assert!(
            CLAUDE_REVIEW_TOOLS
                .split(',')
                .all(|tool| matches!(tool, "Bash" | "Read" | "Glob" | "Grep"))
        );
        assert!(CLAUDE_REVIEW_ALLOWED_TOOLS.iter().all(|tool| {
            !tool.contains("Write")
                && !tool.contains("Edit")
                && !tool.contains("WebFetch")
                && !tool.contains("WebSearch")
        }));
        assert!(!CLAUDE_COMMON_ARGS.contains(&"--safe-mode"));
        assert!(CLAUDE_GRADE_ARGS.contains(&"--safe-mode"));
        assert_eq!(CLAUDE_REVIEW_SETTING_SOURCES, "user,project");
        let settings = serde_json::from_str::<serde_json::Value>(CLAUDE_REVIEW_SETTINGS).unwrap();
        assert_eq!(settings["disableAllHooks"], true);
        assert_eq!(settings["disableAgentView"], true);
        assert_eq!(settings["disableSkillShellExecution"], true);
        assert_eq!(settings["env"]["CLAUDE_CODE_DISABLE_CLAUDE_MDS"], "1");
        assert!(CLAUDE_COMMON_ARGS.contains(&"--strict-mcp-config"));
        assert!(CLAUDE_COMMON_ARGS.contains(&r#"{"mcpServers":{}}"#));
        assert!(CLAUDE_COMMON_ARGS.contains(&"--no-chrome"));
    }

    #[test]
    fn reviewer_prompts_only_invoke_the_project_skill() {
        assert_eq!(CODEX_REVIEW_PROMPT, "$review-single-pr offline-benchmark");
        assert_eq!(CLAUDE_REVIEW_PROMPT, "/review-single-pr offline-benchmark");
        for prompt in [CODEX_REVIEW_PROMPT, CLAUDE_REVIEW_PROMPT] {
            assert!(!prompt.contains("correctness"));
            assert!(!prompt.contains("GitHub"));
            assert!(!prompt.contains("bench-base"));
        }
    }

    #[test]
    fn grader_prompt_limits_context_and_matches_underlying_issues() {
        assert!(GRADE_PROMPT.contains("known_findings.json"));
        assert!(GRADE_PROMPT.contains("candidate_findings.json"));
        assert!(GRADE_PROMPT.contains("same underlying defect or material risk"));
        assert!(GRADE_PROMPT.contains("jointly cover one known finding"));
        assert!(!GRADE_PROMPT.contains("review.json"));
        assert!(!GRADE_PROMPT.contains("match_if"));
    }

    #[test]
    fn claude_schema_removes_unsupported_draft_metadata() {
        let schema = claude_schema(
            r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#,
        )
        .unwrap();
        let schema = serde_json::from_str::<serde_json::Value>(&schema).unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema.get("$schema").is_none());
    }

    #[tokio::test]
    async fn reports_nonzero_and_timeout_processes() {
        let mut failure = Command::new("sh");
        failure.args(["-c", "exit 7"]);
        assert!(run_process(&mut failure, 1, "mock failure").await.is_err());

        let mut slow = Command::new("sh");
        slow.args(["-c", "sleep 2"]);
        let error = run_process(&mut slow, 1, "mock timeout")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("timed out"));
    }

    fn args(command: &Command) -> Vec<&str> {
        command
            .as_std()
            .get_args()
            .map(OsStr::to_str)
            .collect::<Option<Vec<_>>>()
            .unwrap()
    }
}
