use super::common::{git_stdout, run_git, test_workspace};
use crate::support::git::{
    IncrementalPackageSelection, refs::resolve_since_diff_base, select_incremental_packages,
};

#[test]
fn since_tag_resolves_to_commit() {
    let root = tempfile::tempdir().unwrap();
    run_git(root.path(), &["init"]);
    run_git(root.path(), &["config", "user.email", "test@example.com"]);
    run_git(root.path(), &["config", "user.name", "Test User"]);
    std::fs::write(root.path().join("file.txt"), "base\n").unwrap();
    run_git(root.path(), &["add", "file.txt"]);
    run_git(root.path(), &["commit", "-m", "base"]);
    let base = git_stdout(root.path(), &["rev-parse", "HEAD"]);
    run_git(root.path(), &["tag", "base-tag"]);

    std::fs::write(root.path().join("file.txt"), "head\n").unwrap();
    run_git(root.path(), &["commit", "-am", "head"]);

    assert_eq!(
        resolve_since_diff_base(root.path(), "base-tag").unwrap(),
        base
    );
}

#[test]
fn since_ref_that_is_not_head_ancestor_resolves_to_merge_base() {
    let root = tempfile::tempdir().unwrap();
    run_git(root.path(), &["init"]);
    run_git(root.path(), &["config", "user.email", "test@example.com"]);
    run_git(root.path(), &["config", "user.name", "Test User"]);
    std::fs::write(root.path().join("file.txt"), "base\n").unwrap();
    run_git(root.path(), &["add", "file.txt"]);
    run_git(root.path(), &["commit", "-m", "base"]);
    let merge_base = git_stdout(root.path(), &["rev-parse", "HEAD"]);

    run_git(root.path(), &["checkout", "-b", "feature"]);
    std::fs::write(root.path().join("feature.txt"), "feature\n").unwrap();
    run_git(root.path(), &["add", "feature.txt"]);
    run_git(root.path(), &["commit", "-m", "feature"]);

    run_git(root.path(), &["checkout", "-b", "main", &merge_base]);
    std::fs::write(root.path().join("main.txt"), "main\n").unwrap();
    run_git(root.path(), &["add", "main.txt"]);
    run_git(root.path(), &["commit", "-m", "main"]);

    run_git(root.path(), &["checkout", "feature"]);

    assert_eq!(
        resolve_since_diff_base(root.path(), "main").unwrap(),
        merge_base
    );
}

#[test]
fn zero_since_on_new_branch_resolves_to_first_unique_parent() {
    let root = tempfile::tempdir().unwrap();
    run_git(root.path(), &["init"]);
    run_git(root.path(), &["config", "user.email", "test@example.com"]);
    run_git(root.path(), &["config", "user.name", "Test User"]);

    std::fs::write(root.path().join("file.txt"), "base\n").unwrap();
    run_git(root.path(), &["add", "file.txt"]);
    run_git(root.path(), &["commit", "-m", "base"]);
    let base = git_stdout(root.path(), &["rev-parse", "HEAD"]);
    run_git(
        root.path(),
        &["update-ref", "refs/remotes/origin/dev", &base],
    );

    run_git(root.path(), &["checkout", "-b", "feature"]);
    std::fs::write(root.path().join("feature.txt"), "feature 1\n").unwrap();
    run_git(root.path(), &["add", "feature.txt"]);
    run_git(root.path(), &["commit", "-m", "feature 1"]);
    std::fs::write(root.path().join("feature.txt"), "feature 2\n").unwrap();
    run_git(root.path(), &["commit", "-am", "feature 2"]);
    let head = git_stdout(root.path(), &["rev-parse", "HEAD"]);
    run_git(
        root.path(),
        &["update-ref", "refs/remotes/origin/feature", &head],
    );

    assert_eq!(
        resolve_since_diff_base(root.path(), "0000000000000000000000000000000000000000").unwrap(),
        base
    );
}

#[test]
fn zero_since_ignores_default_branch_tip_after_branch_fork() {
    let root = tempfile::tempdir().unwrap();
    run_git(root.path(), &["init"]);
    run_git(root.path(), &["config", "user.email", "test@example.com"]);
    run_git(root.path(), &["config", "user.name", "Test User"]);

    std::fs::write(root.path().join("file.txt"), "base\n").unwrap();
    run_git(root.path(), &["add", "file.txt"]);
    run_git(root.path(), &["commit", "-m", "base"]);
    let fork_point = git_stdout(root.path(), &["rev-parse", "HEAD"]);

    run_git(root.path(), &["checkout", "-b", "feature"]);
    std::fs::write(root.path().join("feature.txt"), "feature\n").unwrap();
    run_git(root.path(), &["add", "feature.txt"]);
    run_git(root.path(), &["commit", "-m", "feature"]);
    let feature_head = git_stdout(root.path(), &["rev-parse", "HEAD"]);

    run_git(root.path(), &["checkout", "-b", "dev", &fork_point]);
    std::fs::write(root.path().join("file.txt"), "dev 1\n").unwrap();
    run_git(root.path(), &["commit", "-am", "dev 1"]);
    std::fs::write(root.path().join("file.txt"), "dev 2\n").unwrap();
    run_git(root.path(), &["commit", "-am", "dev 2"]);
    let dev_head = git_stdout(root.path(), &["rev-parse", "HEAD"]);
    assert_ne!(dev_head, fork_point);
    run_git(
        root.path(),
        &["update-ref", "refs/remotes/origin/dev", &dev_head],
    );

    run_git(root.path(), &["checkout", "feature"]);
    run_git(
        root.path(),
        &["update-ref", "refs/remotes/origin/feature", &feature_head],
    );

    assert_eq!(
        resolve_since_diff_base(root.path(), "0000000000000000000000000000000000000000").unwrap(),
        fork_point
    );
}

#[test]
fn unresolved_push_before_sha_resolves_like_new_branch_since() {
    let root = tempfile::tempdir().unwrap();
    run_git(root.path(), &["init"]);
    run_git(root.path(), &["config", "user.email", "test@example.com"]);
    run_git(root.path(), &["config", "user.name", "Test User"]);

    std::fs::write(root.path().join("file.txt"), "base\n").unwrap();
    run_git(root.path(), &["add", "file.txt"]);
    run_git(root.path(), &["commit", "-m", "base"]);
    let base = git_stdout(root.path(), &["rev-parse", "HEAD"]);
    run_git(
        root.path(),
        &["update-ref", "refs/remotes/origin/dev", &base],
    );

    run_git(root.path(), &["checkout", "-b", "feature"]);
    std::fs::write(root.path().join("feature.txt"), "feature 1\n").unwrap();
    run_git(root.path(), &["add", "feature.txt"]);
    run_git(root.path(), &["commit", "-m", "feature 1"]);
    std::fs::write(root.path().join("feature.txt"), "feature 2\n").unwrap();
    run_git(root.path(), &["commit", "-am", "feature 2"]);
    let head = git_stdout(root.path(), &["rev-parse", "HEAD"]);
    run_git(
        root.path(),
        &["update-ref", "refs/remotes/origin/feature", &head],
    );

    assert_eq!(
        resolve_since_diff_base(root.path(), "1111111111111111111111111111111111111111").unwrap(),
        base
    );
}

#[test]
fn unresolved_push_before_sha_without_remote_refs_returns_error() {
    let root = tempfile::tempdir().unwrap();
    run_git(root.path(), &["init"]);
    run_git(root.path(), &["config", "user.email", "test@example.com"]);
    run_git(root.path(), &["config", "user.name", "Test User"]);

    std::fs::write(root.path().join("file.txt"), "base\n").unwrap();
    run_git(root.path(), &["add", "file.txt"]);
    run_git(root.path(), &["commit", "-m", "base"]);

    let err = resolve_since_diff_base(root.path(), "1111111111111111111111111111111111111111")
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("no remote refs remain"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn unresolved_named_ref_does_not_use_push_before_sha_fallback() {
    let root = tempfile::tempdir().unwrap();
    run_git(root.path(), &["init"]);
    run_git(root.path(), &["config", "user.email", "test@example.com"]);
    run_git(root.path(), &["config", "user.name", "Test User"]);

    std::fs::write(root.path().join("file.txt"), "base\n").unwrap();
    run_git(root.path(), &["add", "file.txt"]);
    run_git(root.path(), &["commit", "-m", "base"]);
    let base = git_stdout(root.path(), &["rev-parse", "HEAD"]);
    run_git(
        root.path(),
        &["update-ref", "refs/remotes/origin/dev", &base],
    );

    let err = resolve_since_diff_base(root.path(), "missing-branch").unwrap_err();

    assert!(
        format!("{err:#}").contains("failed to resolve `missing-branch` to a commit"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn unresolved_short_sha_does_not_use_push_before_sha_fallback() {
    let root = tempfile::tempdir().unwrap();
    run_git(root.path(), &["init"]);
    run_git(root.path(), &["config", "user.email", "test@example.com"]);
    run_git(root.path(), &["config", "user.name", "Test User"]);

    std::fs::write(root.path().join("file.txt"), "base\n").unwrap();
    run_git(root.path(), &["add", "file.txt"]);
    run_git(root.path(), &["commit", "-m", "base"]);
    let base = git_stdout(root.path(), &["rev-parse", "HEAD"]);
    run_git(
        root.path(),
        &["update-ref", "refs/remotes/origin/dev", &base],
    );

    let err = resolve_since_diff_base(root.path(), "1111111").unwrap_err();

    assert!(
        format!("{err:#}").contains("failed to resolve `1111111` to a commit"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn zero_since_root_manifest_change_uses_inferred_base() {
    let (root, metadata, workspace_packages) = test_workspace();
    run_git(root.path(), &["init"]);
    run_git(root.path(), &["config", "user.email", "test.com"]);
    run_git(root.path(), &["config", "user.name", "Test User"]);
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\", \
         \"crates/gamma\"]\n\n[workspace.dependencies]\nalpha = { path = \"crates/alpha\" }\n",
    )
    .unwrap();
    run_git(root.path(), &["add", "."]);
    run_git(root.path(), &["commit", "-m", "base"]);
    let base = git_stdout(root.path(), &["rev-parse", "HEAD"]);
    run_git(
        root.path(),
        &["update-ref", "refs/remotes/origin/dev", &base],
    );

    run_git(root.path(), &["checkout", "-b", "feature"]);
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\", \
         \"crates/gamma\"]\n\n[workspace.dependencies]\nalpha = { path = \"crates/alpha\", \
         package = \"alpha\" }\n",
    )
    .unwrap();
    run_git(root.path(), &["commit", "-am", "feature"]);
    let head = git_stdout(root.path(), &["rev-parse", "HEAD"]);
    run_git(
        root.path(),
        &["update-ref", "refs/remotes/origin/feature", &head],
    );

    let selected = select_incremental_packages(
        root.path(),
        &metadata,
        &workspace_packages,
        "0000000000000000000000000000000000000000",
    )
    .unwrap();

    assert_eq!(
        selected,
        IncrementalPackageSelection::Packages {
            changed: vec!["alpha".into()],
            affected: vec!["alpha".into(), "beta".into(), "gamma".into()],
        }
    );
}

#[test]
fn zero_since_without_remote_refs_returns_error() {
    let root = tempfile::tempdir().unwrap();
    run_git(root.path(), &["init"]);
    run_git(root.path(), &["config", "user.email", "test@example.com"]);
    run_git(root.path(), &["config", "user.name", "Test User"]);
    std::fs::write(root.path().join("file.txt"), "base\n").unwrap();
    run_git(root.path(), &["add", "file.txt"]);
    run_git(root.path(), &["commit", "-m", "base"]);

    let err = resolve_since_diff_base(root.path(), "0000000000000000000000000000000000000000")
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("no remote refs remain"),
        "unexpected error: {err:#}"
    );
}
