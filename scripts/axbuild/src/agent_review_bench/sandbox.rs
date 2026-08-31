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
const PROJECT_SKILLS_PATH: &str = ".agents/skills";

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
    replace_file(&workspace_root.join("AGENTS.md"), &repo.join("AGENTS.md"))
        .context("failed to copy current AGENTS.md into review sandbox")?;
    replace_file(&workspace_root.join("CLAUDE.md"), &repo.join("CLAUDE.md"))
        .context("failed to copy current CLAUDE.md into review sandbox")?;

    let current_skills = workspace_root.join(PROJECT_SKILLS_PATH);
    ensure_directory(&repo.join(".agents"))?;
    ensure_directory(&repo.join(".claude"))?;
    replace_tree(&current_skills, &repo.join(".agents/skills"))?;
    replace_tree(&current_skills, &repo.join(".claude/skills"))?;

    let context_dir = repo.join(".agent-review-context");
    fs::create_dir_all(&context_dir)?;
    fs::write(context_dir.join("reviewer.md"), REVIEW_CONTRACT)?;
    fs::write(context_dir.join("review.schema.json"), REVIEW_SCHEMA)?;
    Ok(())
}

fn replace_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    remove_path(destination)?;
    copy_tree(source, destination)
}

fn replace_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    remove_path(destination)?;
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to copy review context {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn ensure_directory(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => remove_path(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::create_dir_all(path)?;
    Ok(())
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
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
    // The next snapshot may reuse the same size, timestamp, and inode as the
    // base file. Dropping the old index forces Git to hash the new contents.
    git(repo, ["read-tree", "--empty"])?;
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
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;
    use crate::agent_review_bench::cases::{ExpectedFinding, Severity};

    #[test]
    fn creates_standalone_two_commit_repository_without_ground_truth() {
        let workspace = tempdir().unwrap();
        fs::write(workspace.path().join("AGENTS.md"), "current rules\n").unwrap();
        fs::write(workspace.path().join("CLAUDE.md"), "see AGENTS.md\n").unwrap();
        fs::create_dir_all(workspace.path().join(".agents/skills/review-single-pr")).unwrap();
        fs::write(
            workspace
                .path()
                .join(".agents/skills/review-single-pr/SKILL.md"),
            "current review skill\n",
        )
        .unwrap();
        fs::create_dir_all(
            workspace
                .path()
                .join(".agents/skills/rust-code-quality/references"),
        )
        .unwrap();
        fs::write(
            workspace
                .path()
                .join(".agents/skills/rust-code-quality/SKILL.md"),
            "current code-quality skill\n",
        )
        .unwrap();
        fs::write(
            workspace
                .path()
                .join(".agents/skills/rust-code-quality/references/implementation.md"),
            "current implementation rules\n",
        )
        .unwrap();
        fs::create_dir_all(workspace.path().join(".claude/skills/obsolete-skill")).unwrap();
        fs::write(
            workspace
                .path()
                .join(".claude/skills/obsolete-skill/SKILL.md"),
            "obsolete skill\n",
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
            expected: vec![ExpectedFinding {
                id: "secret-finding".into(),
                path: "src/lib.rs".into(),
                line: 1,
                severity: Severity::Major,
                description: "secret answer".into(),
            }],
        };

        let sandbox = ReviewSandbox::create(workspace.path(), &case).unwrap();
        assert!(sandbox.repo().join(".git").is_dir());
        assert!(!sandbox.repo().join(".git/objects/info/alternates").exists());
        assert_eq!(
            fs::read_to_string(sandbox.repo().join("AGENTS.md")).unwrap(),
            "current rules\n"
        );
        assert_eq!(
            fs::read_to_string(sandbox.repo().join("CLAUDE.md")).unwrap(),
            "see AGENTS.md\n"
        );
        for skill_root in [".agents/skills", ".claude/skills"] {
            assert_eq!(
                fs::read_to_string(
                    sandbox
                        .repo()
                        .join(skill_root)
                        .join("review-single-pr/SKILL.md")
                )
                .unwrap(),
                "current review skill\n"
            );
            assert_eq!(
                fs::read_to_string(
                    sandbox
                        .repo()
                        .join(skill_root)
                        .join("rust-code-quality/SKILL.md")
                )
                .unwrap(),
                "current code-quality skill\n"
            );
            assert_eq!(
                fs::read_to_string(
                    sandbox
                        .repo()
                        .join(skill_root)
                        .join("rust-code-quality/references/implementation.md")
                )
                .unwrap(),
                "current implementation rules\n"
            );
            assert!(
                !sandbox
                    .repo()
                    .join(skill_root)
                    .join("obsolete-skill")
                    .exists()
            );
        }
        assert!(
            fs::symlink_metadata(sandbox.repo().join(".claude/skills/review-single-pr"))
                .unwrap()
                .is_dir()
        );
        assert!(!sandbox.repo().join("docs").join("guideline").exists());
        let diff = Command::new("git")
            .current_dir(sandbox.repo())
            .args(["diff", "bench-base", "HEAD", "--", "src/lib.rs"])
            .output()
            .unwrap();
        let diff = String::from_utf8(diff.stdout).unwrap();
        assert!(diff.contains("value() -> u8 { 2 }"));
        let changed_paths = Command::new("git")
            .current_dir(sandbox.repo())
            .args(["diff", "--name-only", "bench-base", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(changed_paths.stdout).unwrap(),
            "src/lib.rs\n"
        );
        assert!(!WalkDir::new(sandbox.repo()).into_iter().any(|entry| {
            entry
                .ok()
                .and_then(|entry| fs::read_to_string(entry.path()).ok())
                .is_some_and(|text| text.contains("secret answer"))
        }));
    }

    #[test]
    fn clear_worktree_drops_index_state_before_extracting_the_next_snapshot() {
        let repo = tempdir().unwrap();
        initialize_repo(repo.path()).unwrap();
        fs::write(repo.path().join("tracked.txt"), "base snapshot\n").unwrap();
        commit_all(repo.path(), "base").unwrap();

        clear_worktree(repo.path()).unwrap();

        let cached_paths = Command::new("git")
            .current_dir(repo.path())
            .args(["ls-files", "--cached"])
            .output()
            .unwrap();
        assert!(cached_paths.status.success());
        assert!(cached_paths.stdout.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn context_replacement_unlinks_destinations_without_touching_external_targets() {
        let root = tempdir().unwrap();
        let external = tempdir().unwrap();

        let source_file = root.path().join("current-agents.md");
        fs::write(&source_file, "current rules\n").unwrap();
        let external_file = external.path().join("historical-agents.md");
        fs::write(&external_file, "external rules\n").unwrap();
        let destination_file = root.path().join("AGENTS.md");
        symlink(&external_file, &destination_file).unwrap();

        replace_file(&source_file, &destination_file).unwrap();

        assert_eq!(
            fs::read_to_string(&destination_file).unwrap(),
            "current rules\n"
        );
        assert_eq!(
            fs::read_to_string(&external_file).unwrap(),
            "external rules\n"
        );
        assert!(
            !fs::symlink_metadata(&destination_file)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        let source_tree = root.path().join("current-skills");
        fs::create_dir(&source_tree).unwrap();
        fs::write(source_tree.join("SKILL.md"), "current skill\n").unwrap();
        let external_tree = external.path().join("historical-skills");
        fs::create_dir(&external_tree).unwrap();
        fs::write(external_tree.join("SKILL.md"), "external skill\n").unwrap();
        let destination_tree = root.path().join("skills");
        symlink(&external_tree, &destination_tree).unwrap();

        replace_tree(&source_tree, &destination_tree).unwrap();

        assert_eq!(
            fs::read_to_string(destination_tree.join("SKILL.md")).unwrap(),
            "current skill\n"
        );
        assert_eq!(
            fs::read_to_string(external_tree.join("SKILL.md")).unwrap(),
            "external skill\n"
        );
        assert!(fs::symlink_metadata(&destination_tree).unwrap().is_dir());
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
