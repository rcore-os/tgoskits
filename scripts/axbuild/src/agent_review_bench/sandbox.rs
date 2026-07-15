use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use tempfile::TempDir;
use walkdir::WalkDir;

use super::cases::BenchCase;

const REVIEW_CONTRACT: &str = include_str!("../../../agent-review-bench/reviewer.md");
const REVIEW_SCHEMA: &str = include_str!("../../../agent-review-bench/schemas/review.schema.json");

pub(super) struct ReviewSandbox {
    _root: TempDir,
    repo: PathBuf,
}

impl ReviewSandbox {
    pub(super) fn create(workspace_root: &Path, case: &BenchCase) -> anyhow::Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("tgos-agent-review-")
            .tempdir()
            .context("failed to create review sandbox")?;
        let repo = root.path().join("repo");
        fs::create_dir(&repo)?;

        extract_snapshot(workspace_root, &case.base, root.path(), &repo)?;
        overlay_current_review_context(workspace_root, &repo)?;
        initialize_repo(&repo)?;
        commit_all(&repo, "benchmark base")?;
        git(&repo, ["branch", "bench-base"])?;

        clear_worktree(&repo)?;
        extract_snapshot(workspace_root, &case.head, root.path(), &repo)?;
        overlay_current_review_context(workspace_root, &repo)?;
        commit_all(&repo, &case.title)?;
        ensure_review_diff(&repo)?;
        ensure_standalone_git_dir(&repo)?;

        Ok(Self { _root: root, repo })
    }

    pub(super) fn repo(&self) -> &Path {
        &self.repo
    }

    pub(super) fn review_schema(&self) -> PathBuf {
        self.repo.join(".agent-review-context/review.schema.json")
    }

    pub(super) fn temporary_review_output(&self) -> PathBuf {
        self.repo
            .parent()
            .expect("sandbox repository must have a parent")
            .join("review.json")
    }
}

fn extract_snapshot(
    workspace_root: &Path,
    revision: &str,
    scratch_root: &Path,
    destination: &Path,
) -> anyhow::Result<()> {
    let archive_path = scratch_root.join(format!("{revision}.tar"));
    let status = Command::new("git")
        .current_dir(workspace_root)
        .arg("archive")
        .arg("--format=tar")
        .arg(format!("--output={}", archive_path.display()))
        .arg(revision)
        .status()
        .with_context(|| format!("failed to archive revision {revision}"))?;
    if !status.success() {
        bail!("git archive exited with status {status} for revision {revision}");
    }

    let archive_file = fs::File::open(&archive_path)?;
    tar::Archive::new(archive_file)
        .unpack(destination)
        .with_context(|| format!("failed to unpack revision {revision}"))?;
    fs::remove_file(archive_path)?;
    Ok(())
}

fn overlay_current_review_context(workspace_root: &Path, repo: &Path) -> anyhow::Result<()> {
    fs::copy(workspace_root.join("AGENTS.md"), repo.join("AGENTS.md"))
        .context("failed to copy current AGENTS.md into review sandbox")?;

    let guideline_destination = repo.join("book/guideline");
    if guideline_destination.exists() {
        fs::remove_dir_all(&guideline_destination)?;
    }
    copy_tree(
        &workspace_root.join("book/guideline"),
        &guideline_destination,
    )?;

    let context_dir = repo.join(".agent-review-context");
    fs::create_dir_all(&context_dir)?;
    fs::write(context_dir.join("reviewer.md"), REVIEW_CONTRACT)?;
    fs::write(context_dir.join("review.schema.json"), REVIEW_SCHEMA)?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    for entry in WalkDir::new(source) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "failed to copy review context {} to {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        } else {
            bail!(
                "unsupported review-context file type at {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

fn initialize_repo(repo: &Path) -> anyhow::Result<()> {
    git(repo, ["init", "--quiet"])?;
    git(repo, ["config", "user.name", "TGOS Review Benchmark"])?;
    git(repo, ["config", "user.email", "review-benchmark@invalid"])?;
    Ok(())
}

fn commit_all(repo: &Path, message: &str) -> anyhow::Result<()> {
    git(repo, ["add", "--all"])?;
    git(repo, ["commit", "--quiet", "--message", message])?;
    Ok(())
}

fn clear_worktree(repo: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(repo)? {
        let entry = entry?;
        if entry.file_name() == OsStr::new(".git") {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn ensure_review_diff(repo: &Path) -> anyhow::Result<()> {
    let status = Command::new("git")
        .current_dir(repo)
        .args(["diff", "--quiet", "bench-base", "HEAD", "--"])
        .status()?;
    match status.code() {
        Some(1) => Ok(()),
        Some(0) => bail!("synthetic review repository has an empty diff"),
        _ => bail!("git diff exited with status {status}"),
    }
}

fn ensure_standalone_git_dir(repo: &Path) -> anyhow::Result<()> {
    let git_dir = repo.join(".git");
    if !git_dir.is_dir() {
        bail!("review sandbox .git is not a standalone directory");
    }
    let alternates = git_dir.join("objects/info/alternates");
    if alternates.exists() {
        bail!("review sandbox unexpectedly references external Git objects");
    }
    Ok(())
}

fn git<I, S>(repo: &Path, args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .context("failed to spawn git")?;
    if status.success() {
        Ok(())
    } else {
        bail!("git exited with status {status} in {}", repo.display())
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;
    use crate::agent_review_bench::cases::{ExpectedFinding, Severity};

    #[test]
    fn creates_standalone_two_commit_repository_without_ground_truth() {
        let workspace = tempdir().unwrap();
        fs::write(workspace.path().join("AGENTS.md"), "current rules\n").unwrap();
        fs::create_dir_all(workspace.path().join("book/guideline")).unwrap();
        fs::write(
            workspace.path().join("book/guideline/code-quality.md"),
            "current guideline\n",
        )
        .unwrap();
        git(workspace.path(), ["init", "--quiet"]).unwrap();
        git(workspace.path(), ["config", "user.name", "test"]).unwrap();
        git(workspace.path(), ["config", "user.email", "test@invalid"]).unwrap();
        fs::create_dir_all(workspace.path().join("src")).unwrap();
        fs::write(
            workspace.path().join("src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .unwrap();
        commit_all(workspace.path(), "base").unwrap();
        let base = rev_parse(workspace.path(), "HEAD");
        fs::write(
            workspace.path().join("src/lib.rs"),
            "pub fn value() -> u8 { 2 }\n",
        )
        .unwrap();
        commit_all(workspace.path(), "head").unwrap();
        let head = rev_parse(workspace.path(), "HEAD");

        let case = BenchCase {
            id: "0001-sample".into(),
            pr: 1,
            title: "sample change".into(),
            remote: "https://github.com/example/repo.git".into(),
            base,
            head,
            source: "secret source".into(),
            fixed_by: "a".repeat(40),
            expected: vec![ExpectedFinding {
                id: "secret-finding".into(),
                path: "src/lib.rs".into(),
                line: 1,
                severity: Severity::Major,
                description: "secret answer".into(),
                match_if: "secret criterion".into(),
            }],
        };

        let sandbox = ReviewSandbox::create(workspace.path(), &case).unwrap();
        assert!(sandbox.repo().join(".git").is_dir());
        assert!(!sandbox.repo().join(".git/objects/info/alternates").exists());
        assert_eq!(
            fs::read_to_string(sandbox.repo().join("AGENTS.md")).unwrap(),
            "current rules\n"
        );
        let diff = Command::new("git")
            .current_dir(sandbox.repo())
            .args(["diff", "bench-base", "HEAD", "--", "src/lib.rs"])
            .output()
            .unwrap();
        let diff = String::from_utf8(diff.stdout).unwrap();
        assert!(diff.contains("value() -> u8 { 2 }"));
        assert!(!WalkDir::new(sandbox.repo()).into_iter().any(|entry| {
            entry
                .ok()
                .and_then(|entry| fs::read_to_string(entry.path()).ok())
                .is_some_and(|text| text.contains("secret answer"))
        }));
    }

    fn rev_parse(repo: &Path, revision: &str) -> String {
        let output = Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", revision])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }
}
