use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

const ZERO_SINCE_REF: &str = "0000000000000000000000000000000000000000";

pub(crate) fn changed_paths_since(
    workspace_root: &Path,
    since: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    changed_paths_since_with_base(workspace_root, since).map(|(paths, _)| paths)
}

pub(super) fn changed_paths_since_with_base(
    workspace_root: &Path,
    since: &str,
) -> anyhow::Result<(Vec<PathBuf>, String)> {
    ensure_git_work_tree(workspace_root)?;

    let diff_base = resolve_since_diff_base(workspace_root, since)?;

    // Three-dot `<base>...HEAD` diffs against the merge-base, so it captures
    // only what this branch changed since it forked from `base`. Two-dot would
    // also surface commits made on the base side after the fork point, which
    // over-selects packages and can spuriously trip the global-input full
    // fallback (e.g. a toolchain bump that landed on the base branch).
    let range = format!("{diff_base}...HEAD");
    let output = Command::new("git")
        .args(git_safe_directory_args(workspace_root))
        .arg("-C")
        .arg(workspace_root)
        .args(["diff", "--name-only", range.as_str(), "--"])
        .output()
        .with_context(|| format!("failed to run git diff for `{range}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git diff exited with status {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    Ok((
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect(),
        diff_base,
    ))
}

pub(super) fn resolve_since_diff_base(
    workspace_root: &Path,
    since: &str,
) -> anyhow::Result<String> {
    if since.is_empty() {
        bail!("since ref is empty");
    }
    if since == ZERO_SINCE_REF {
        let diff_base = infer_zero_since_diff_base(workspace_root)
            .context("failed to infer diff base for zero since ref")?;
        println!("input ref `{since}` is zero; inferred `{diff_base}` as incremental diff base");
        return Ok(diff_base);
    }

    let since_commit = match git_commit_for_ref(workspace_root, since) {
        Ok(commit) => commit,
        Err(err) if is_unresolved_commit_sha_candidate(since) => {
            let diff_base = infer_zero_since_diff_base(workspace_root).with_context(|| {
                format!(
                    "failed to infer diff base for unresolved input ref `{since}` after commit \
                     resolution failed: {err:#}"
                )
            })?;
            println!(
                "input ref `{since}` could not be resolved; inferred `{diff_base}` as incremental \
                 diff base"
            );
            return Ok(diff_base);
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to resolve `{since}` to a commit"));
        }
    };
    if git_ref_is_ancestor_of_head(workspace_root, &since_commit)? {
        println!("using input ref `{since}` (`{since_commit}`) as incremental diff base");
        return Ok(since_commit);
    }

    let merge_base = git_merge_base_with_head(workspace_root, &since_commit)
        .with_context(|| format!("failed to find merge-base between `{since}` and HEAD"))?;
    println!(
        "input ref `{since}` (`{since_commit}`) is not an ancestor of HEAD; using merge-base \
         `{merge_base}` as incremental diff base"
    );
    Ok(merge_base)
}

fn is_unresolved_commit_sha_candidate(since: &str) -> bool {
    since.len() == 40
        && since != ZERO_SINCE_REF
        && since.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn infer_zero_since_diff_base(workspace_root: &Path) -> anyhow::Result<String> {
    let head_commit = git_commit_for_ref(workspace_root, "HEAD")
        .context("failed to resolve HEAD for zero since inference")?;
    let remote_refs = git_remote_refs_not_at_commit(workspace_root, &head_commit)
        .context("failed to list remote refs for zero since inference")?;
    if remote_refs.is_empty() {
        bail!("no remote refs remain after excluding refs at HEAD");
    }

    let mut args = vec!["rev-list", "--reverse", "--parents", "HEAD", "--not"];
    args.extend(remote_refs.iter().map(String::as_str));
    let output = Command::new("git")
        .args(git_safe_directory_args(workspace_root))
        .arg("-C")
        .arg(workspace_root)
        .args(args)
        .output()
        .context("failed to run git rev-list for zero since inference")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git rev-list exited with status {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    let first_unique = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
        .context("HEAD has no commits outside remote refs not at HEAD")?;
    let mut parts = first_unique.split_whitespace();
    let commit = parts
        .next()
        .context("git rev-list returned an empty line")?;
    let parent = parts
        .next()
        .with_context(|| format!("first unique commit `{commit}` has no parent"))?;

    git_commit_for_ref(workspace_root, parent)
        .with_context(|| format!("failed to resolve inferred parent `{parent}` to a commit"))
}

fn git_remote_refs_not_at_commit(
    workspace_root: &Path,
    excluded_commit: &str,
) -> anyhow::Result<Vec<String>> {
    let output = Command::new("git")
        .args(git_safe_directory_args(workspace_root))
        .arg("-C")
        .arg(workspace_root)
        .args([
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/remotes",
        ])
        .output()
        .context("failed to run git for-each-ref for remote refs")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git for-each-ref exited with status {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    let refs = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let git_ref = parts.next()?;
            let commit = parts.next()?;
            (commit != excluded_commit).then(|| git_ref.to_string())
        })
        .collect();
    Ok(refs)
}

fn git_commit_for_ref(workspace_root: &Path, git_ref: &str) -> anyhow::Result<String> {
    let commit_ref = format!("{git_ref}^{{commit}}");
    let output = Command::new("git")
        .args(git_safe_directory_args(workspace_root))
        .arg("-C")
        .arg(workspace_root)
        .args(["rev-parse", "--verify", commit_ref.as_str()])
        .output()
        .with_context(|| format!("failed to resolve `{git_ref}` to a commit"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git rev-parse exited with status {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if commit.is_empty() {
        bail!("git rev-parse returned an empty commit for `{git_ref}`");
    }
    Ok(commit)
}

fn git_ref_is_ancestor_of_head(workspace_root: &Path, git_ref: &str) -> anyhow::Result<bool> {
    let output = Command::new("git")
        .args(git_safe_directory_args(workspace_root))
        .arg("-C")
        .arg(workspace_root)
        .args(["merge-base", "--is-ancestor", git_ref, "HEAD"])
        .output()
        .with_context(|| format!("failed to check whether `{git_ref}` is an ancestor of HEAD"))?;

    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            bail!(
                "git merge-base --is-ancestor exited with status {}{}",
                output.status,
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            )
        }
    }
}

fn git_merge_base_with_head(workspace_root: &Path, git_ref: &str) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(git_safe_directory_args(workspace_root))
        .arg("-C")
        .arg(workspace_root)
        .args(["merge-base", git_ref, "HEAD"])
        .output()
        .with_context(|| format!("failed to run git merge-base for `{git_ref}` and HEAD"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git merge-base exited with status {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    let merge_base = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if merge_base.is_empty() {
        bail!("git merge-base returned an empty base for `{git_ref}` and HEAD");
    }
    Ok(merge_base)
}

fn ensure_git_work_tree(workspace_root: &Path) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(git_safe_directory_args(workspace_root))
        .arg("-C")
        .arg(workspace_root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .with_context(|| {
            format!(
                "failed to check whether {} is a git work tree",
                workspace_root.display()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "{} is not a git work tree{}",
            workspace_root.display(),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim() != "true" {
        bail!("{} is not inside a git work tree", workspace_root.display());
    }

    Ok(())
}

pub(super) fn git_safe_directory_args(workspace_root: &Path) -> [String; 2] {
    [
        "-c".to_string(),
        format!("safe.directory={}", workspace_root.display()),
    ]
}
