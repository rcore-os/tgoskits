use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, bail};
use tempfile::tempdir;
use tokio::{process::Command, time::timeout};

use super::{cases::BenchCase, sandbox::ReviewSandbox};

const REVIEW_PROMPT: &str =
    "You are performing an offline code review benchmark. Review only the committed changes from \
     `bench-base` to `HEAD` in the current repository. First read `AGENTS.md`, every file under \
     `book/guideline/`, and `.agent-review-context/reviewer.md`, then follow those rules. Inspect \
     the diff and relevant in-repository context for actionable defects. Do not use the network, \
     GitHub, paths outside this repository, or write operations. Return only the JSON object \
     required by the output schema.";
const GRADE_PROMPT: &str = "You are grading an offline code review. Read `expected.json` and \
                            `review.json` in the current directory. For every expected item, \
                            decide whether one review finding satisfies its `match_if` criterion \
                            at the stated code location. Return exactly one match object per \
                            expected ID. Use the zero-based index in `review.findings` when \
                            caught, or null when missed. Wording may differ, but nearby or \
                            generic comments do not count. Do not inspect any other paths.";
const GRADE_SCHEMA: &str = include_str!("../../../agent-review-bench/schemas/grade.schema.json");

pub(super) struct CodexOptions {
    pub(super) model: Option<String>,
    pub(super) reasoning_effort: String,
    pub(super) timeout_secs: u64,
}

pub(super) struct CodexRunner {
    program: PathBuf,
}

impl CodexRunner {
    pub(super) fn discover() -> anyhow::Result<Self> {
        let runner = Self {
            program: PathBuf::from("codex"),
        };
        runner
            .version()
            .context("Codex CLI is unavailable or not authenticated for local use")?;
        Ok(runner)
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
        let version = String::from_utf8(output.stdout).context("Codex version was not UTF-8")?;
        let version = version.trim();
        if version.is_empty() {
            bail!("Codex CLI returned an empty version");
        }
        Ok(version.to_string())
    }

    pub(super) async fn review(
        &self,
        sandbox: &ReviewSandbox,
        artifact_path: &Path,
        options: &CodexOptions,
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
            .arg(&temporary_output);
        apply_model_options(&mut command, options);
        command.arg(REVIEW_PROMPT);

        run_process(&mut command, options.timeout_secs, "Codex reviewer").await?;
        copy_nonempty_output(&temporary_output, artifact_path, "reviewer")
    }

    pub(super) async fn grade(
        &self,
        case: &BenchCase,
        review_path: &Path,
        artifact_path: &Path,
        options: &CodexOptions,
    ) -> anyhow::Result<()> {
        let grader_dir = tempdir().context("failed to create isolated grader directory")?;
        let grader_review = grader_dir.path().join("review.json");
        fs::copy(review_path, &grader_review)?;
        let expected = serde_json::to_string_pretty(&case.expected)?;
        fs::write(grader_dir.path().join("expected.json"), expected)?;
        let schema_path = grader_dir.path().join("grade.schema.json");
        fs::write(&schema_path, GRADE_SCHEMA)?;
        let temporary_output = grader_dir.path().join("grade.json");

        let mut command = Command::new(&self.program);
        command
            .args(["exec", "--skip-git-repo-check", "--ephemeral"])
            .args(["--sandbox", "read-only"])
            .args(["-c", "approval_policy=\"never\""])
            .args(["-c", "sandbox_workspace_write.network_access=false"])
            .arg("--cd")
            .arg(grader_dir.path())
            .arg("--output-schema")
            .arg(&schema_path)
            .arg("--output-last-message")
            .arg(&temporary_output);
        apply_model_options(&mut command, options);
        command.arg(GRADE_PROMPT);

        run_process(&mut command, options.timeout_secs, "Codex grader").await?;
        copy_nonempty_output(&temporary_output, artifact_path, "grader")
    }

    #[cfg(test)]
    pub(super) fn from_program(program: PathBuf) -> Self {
        Self { program }
    }
}

fn apply_model_options(command: &mut Command, options: &CodexOptions) {
    if let Some(model) = &options.model {
        command.args(["--model", model]);
    }
    command.args([
        "-c",
        &format!("model_reasoning_effort=\"{}\"", options.reasoning_effort),
    ]);
}

async fn run_process(
    command: &mut Command,
    timeout_secs: u64,
    description: &str,
) -> anyhow::Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
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

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn reads_version_from_injected_codex_program() {
        let temp = tempdir().unwrap();
        let program = temp.path().join("mock-codex");
        fs::write(&program, "#!/bin/sh\necho 'codex-cli test-version'\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CodexRunner::from_program(program);
        assert_eq!(runner.version().unwrap(), "codex-cli test-version");
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
}
