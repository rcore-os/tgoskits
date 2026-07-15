use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
    process::Command,
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

const CASES_DIR: &str = "scripts/agent-review-bench/cases";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BenchCase {
    pub(super) id: String,
    pub(super) pr: u64,
    pub(super) title: String,
    pub(super) remote: String,
    pub(super) base: String,
    pub(super) head: String,
    pub(super) source: String,
    pub(super) fixed_by: String,
    pub(super) expected: Vec<ExpectedFinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExpectedFinding {
    pub(super) id: String,
    pub(super) path: String,
    pub(super) line: usize,
    pub(super) severity: Severity,
    pub(super) description: String,
    pub(super) match_if: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Severity {
    Critical,
    Major,
    Minor,
    Nit,
}

pub(super) fn load_cases(workspace_root: &Path) -> anyhow::Result<Vec<BenchCase>> {
    let cases_dir = workspace_root.join(CASES_DIR);
    let mut paths = fs::read_dir(&cases_dir)
        .with_context(|| {
            format!(
                "failed to read benchmark cases from {}",
                cases_dir.display()
            )
        })?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "toml")
    });
    paths.sort();

    let mut cases = Vec::with_capacity(paths.len());
    for path in paths {
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let case = toml::from_str::<BenchCase>(&text)
            .with_context(|| format!("invalid benchmark case {}", path.display()))?;
        validate_case_schema(&case)
            .with_context(|| format!("invalid benchmark case {}", path.display()))?;
        cases.push(case);
    }
    validate_unique_ids(&cases)?;
    if cases.is_empty() {
        bail!(
            "no benchmark case TOML files found in {}",
            cases_dir.display()
        );
    }
    Ok(cases)
}

pub(super) fn select_cases<'a>(
    cases: &'a [BenchCase],
    case_ids: &[String],
    prs: &[u64],
) -> anyhow::Result<Vec<&'a BenchCase>> {
    if case_ids.is_empty() && prs.is_empty() {
        return Ok(cases.iter().collect());
    }

    let requested_ids = case_ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let requested_prs = prs.iter().copied().collect::<BTreeSet<_>>();
    let selected = cases
        .iter()
        .filter(|case| requested_ids.contains(case.id.as_str()) || requested_prs.contains(&case.pr))
        .collect::<Vec<_>>();

    let found_ids = selected
        .iter()
        .map(|case| case.id.as_str())
        .collect::<BTreeSet<_>>();
    let found_prs = selected.iter().map(|case| case.pr).collect::<BTreeSet<_>>();
    let missing_ids = requested_ids
        .difference(&found_ids)
        .copied()
        .collect::<Vec<_>>();
    let missing_prs = requested_prs
        .difference(&found_prs)
        .copied()
        .collect::<Vec<_>>();
    if !missing_ids.is_empty() || !missing_prs.is_empty() {
        bail!(
            "unknown benchmark selectors: case IDs [{}], PRs [{}]",
            missing_ids.join(", "),
            missing_prs
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(selected)
}

pub(super) fn prepare_case(workspace_root: &Path, case: &BenchCase) -> anyhow::Result<()> {
    ensure_commit(workspace_root, case, &case.base)?;
    ensure_commit(workspace_root, case, &case.head)?;
    ensure_commit(workspace_root, case, &case.fixed_by)?;
    ensure_ancestor(workspace_root, &case.base, &case.head)?;

    let changed_paths = git_lines(
        workspace_root,
        &["diff", "--name-only", &case.base, &case.head, "--"],
    )?
    .into_iter()
    .collect::<BTreeSet<_>>();
    for expected in &case.expected {
        if !changed_paths.contains(&expected.path) {
            bail!(
                "expected finding `{}` targets `{}`, which is not changed by {}..{}",
                expected.id,
                expected.path,
                case.base,
                case.head
            );
        }
        let object = format!("{}:{}", case.head, expected.path);
        let content = git_output(workspace_root, &["show", &object])?;
        let line_count = content.lines().count();
        if expected.line > line_count {
            bail!(
                "expected finding `{}` line {} exceeds `{}` line count {} at {}",
                expected.id,
                expected.line,
                expected.path,
                line_count,
                case.head
            );
        }
        if !line_is_in_head_hunk(workspace_root, case, expected)? {
            bail!(
                "expected finding `{}` line {} is not a HEAD-side line in a changed hunk of `{}`",
                expected.id,
                expected.line,
                expected.path
            );
        }
    }
    Ok(())
}

fn validate_case_schema(case: &BenchCase) -> anyhow::Result<()> {
    if !valid_id(&case.id) {
        bail!("case id must contain only lowercase ASCII letters, digits, and hyphens");
    }
    if case.pr == 0 {
        bail!("PR number must be greater than zero");
    }
    for (name, value) in [
        ("title", case.title.as_str()),
        ("remote", case.remote.as_str()),
        ("source", case.source.as_str()),
    ] {
        if value.trim().is_empty() {
            bail!("{name} must not be empty");
        }
    }
    if !(case.remote.starts_with("https://") || case.remote.starts_with("git@")) {
        bail!("remote must be an https:// or git@ fetch URL");
    }
    for (name, sha) in [
        ("base", case.base.as_str()),
        ("head", case.head.as_str()),
        ("fixed_by", case.fixed_by.as_str()),
    ] {
        if !valid_sha(sha) {
            bail!("{name} must be a full 40-character lowercase hexadecimal SHA");
        }
    }
    if case.base == case.head {
        bail!("base and head must differ");
    }
    if case.expected.is_empty() {
        bail!("at least one expected finding is required");
    }

    let mut finding_ids = BTreeSet::new();
    for expected in &case.expected {
        if !valid_id(&expected.id) {
            bail!("finding id `{}` is invalid", expected.id);
        }
        if !finding_ids.insert(expected.id.as_str()) {
            bail!("duplicate finding id `{}`", expected.id);
        }
        let finding_path = Path::new(&expected.path);
        if expected.path.is_empty()
            || finding_path.is_absolute()
            || !finding_path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            bail!("finding `{}` path must be repository-relative", expected.id);
        }
        if expected.line == 0 {
            bail!("finding `{}` line must be greater than zero", expected.id);
        }
        if expected.description.trim().is_empty() || expected.match_if.trim().is_empty() {
            bail!(
                "finding `{}` description and match_if must not be empty",
                expected.id
            );
        }
    }
    Ok(())
}

fn validate_unique_ids(cases: &[BenchCase]) -> anyhow::Result<()> {
    let mut case_ids = BTreeSet::new();
    let mut finding_ids = BTreeSet::new();
    for case in cases {
        if !case_ids.insert(case.id.as_str()) {
            bail!("duplicate benchmark case id `{}`", case.id);
        }
        for expected in &case.expected {
            if !finding_ids.insert(expected.id.as_str()) {
                bail!("duplicate benchmark finding id `{}`", expected.id);
            }
        }
    }
    Ok(())
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn ensure_commit(workspace_root: &Path, case: &BenchCase, sha: &str) -> anyhow::Result<()> {
    if git_status(
        workspace_root,
        &["cat-file", "-e", &format!("{sha}^{{commit}}")],
    )? {
        return Ok(());
    }
    let direct_fetch = Command::new("git")
        .current_dir(workspace_root)
        .args(["fetch", "--no-tags", &case.remote, sha])
        .output()
        .with_context(|| format!("failed to fetch commit {sha} from {}", case.remote))?;
    if !direct_fetch.status.success() {
        let pull_ref = format!("refs/pull/{}/head", case.pr);
        let pull_fetch = Command::new("git")
            .current_dir(workspace_root)
            .args(["fetch", "--no-tags", &case.remote, &pull_ref])
            .output()
            .with_context(|| format!("failed to fetch {pull_ref} from {}", case.remote))?;
        if !pull_fetch.status.success() {
            bail!(
                "could not fetch commit {sha} directly or through {pull_ref}: direct fetch: {}; \
                 PR fetch: {}",
                String::from_utf8_lossy(&direct_fetch.stderr).trim(),
                String::from_utf8_lossy(&pull_fetch.stderr).trim()
            );
        }
    }
    if !git_status(
        workspace_root,
        &["cat-file", "-e", &format!("{sha}^{{commit}}")],
    )? {
        bail!("fetched object {sha} is not a commit");
    }
    Ok(())
}

fn line_is_in_head_hunk(
    workspace_root: &Path,
    case: &BenchCase,
    expected: &ExpectedFinding,
) -> anyhow::Result<bool> {
    let diff = git_output(
        workspace_root,
        &[
            "diff",
            "--unified=1",
            &case.base,
            &case.head,
            "--",
            &expected.path,
        ],
    )?;
    for line in diff.lines().filter(|line| line.starts_with("@@ ")) {
        let Some(head_range) = line
            .split_ascii_whitespace()
            .find(|field| field.starts_with('+'))
        else {
            continue;
        };
        let range = head_range.trim_start_matches('+');
        let (start, count) = match range.split_once(',') {
            Some((start, count)) => (start, count),
            None => (range, "1"),
        };
        let start = start
            .parse::<usize>()
            .with_context(|| format!("invalid diff hunk start `{start}`"))?;
        let count = count
            .parse::<usize>()
            .with_context(|| format!("invalid diff hunk count `{count}`"))?;
        if count > 0 && (start..start + count).contains(&expected.line) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_ancestor(workspace_root: &Path, base: &str, head: &str) -> anyhow::Result<()> {
    if git_status(workspace_root, &["merge-base", "--is-ancestor", base, head])? {
        Ok(())
    } else {
        bail!("base {base} is not an ancestor of head {head}")
    }
}

fn git_lines(workspace_root: &Path, args: &[&str]) -> anyhow::Result<Vec<String>> {
    Ok(git_output(workspace_root, args)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn git_output(workspace_root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git {} exited with status {}: {}",
            args.join(" "),
            output.status,
            stderr.trim()
        );
    }
    String::from_utf8(output.stdout).context("git output was not UTF-8")
}

fn git_status(workspace_root: &Path, args: &[&str]) -> anyhow::Result<bool> {
    let status = Command::new("git")
        .current_dir(workspace_root)
        .args(args)
        .status()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn sample_case() -> BenchCase {
        BenchCase {
            id: "0001-sample".into(),
            pr: 1,
            title: "sample".into(),
            remote: "https://github.com/example/repo.git".into(),
            base: "a".repeat(40),
            head: "b".repeat(40),
            source: "https://github.com/example/repo/pull/1".into(),
            fixed_by: "c".repeat(40),
            expected: vec![ExpectedFinding {
                id: "sample-finding".into(),
                path: "src/lib.rs".into(),
                line: 1,
                severity: Severity::Major,
                description: "sample defect".into(),
                match_if: "reviewer identifies sample defect".into(),
            }],
        }
    }

    #[test]
    fn validates_well_formed_case() {
        validate_case_schema(&sample_case()).unwrap();
    }

    #[test]
    fn rejects_short_sha() {
        let mut case = sample_case();
        case.head = "abc".into();
        assert!(validate_case_schema(&case).is_err());
    }

    #[test]
    fn selectors_form_a_deduplicated_union() {
        let first = sample_case();
        let mut second = sample_case();
        second.id = "0002-second".into();
        second.pr = 2;
        second.expected[0].id = "second-finding".into();
        let cases = [first, second];

        let selected = select_cases(
            &cases,
            &["0001-sample".into(), "0001-sample".into()],
            &[2, 2],
        )
        .unwrap();
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn rejects_unknown_selector() {
        assert!(select_cases(&[sample_case()], &["missing".into()], &[]).is_err());
    }

    #[test]
    fn accepts_head_context_line_adjacent_to_deletion() {
        let (repo, case) = case_with_file_change(
            "setting = true\ntimeout = 300\nfail_regex = []\n",
            "setting = true\nfail_regex = []\n",
            2,
        );

        assert!(line_is_in_head_hunk(repo.path(), &case, &case.expected[0]).unwrap());
    }

    #[test]
    fn accepts_added_head_line() {
        let (repo, case) = case_with_file_change(
            "setting = true\nfail_regex = []\n",
            "setting = true\ntimeout = 300\nfail_regex = []\n",
            2,
        );

        assert!(line_is_in_head_hunk(repo.path(), &case, &case.expected[0]).unwrap());
    }

    #[test]
    fn rejects_unchanged_head_line_outside_diff_hunk() {
        let (repo, case) = case_with_file_change(
            "setting = true\ntimeout = 300\nfirst = 1\nsecond = 2\nthird = 3\n",
            "setting = true\nfirst = 1\nsecond = 2\nthird = 3\n",
            4,
        );

        assert!(!line_is_in_head_hunk(repo.path(), &case, &case.expected[0]).unwrap());
    }

    fn case_with_file_change(
        base_content: &str,
        head_content: &str,
        expected_line: usize,
    ) -> (tempfile::TempDir, BenchCase) {
        let repo = tempdir().unwrap();
        initialize_repo(repo.path());
        let base = commit_file(repo.path(), base_content, "base");
        let head = commit_file(repo.path(), head_content, "change file");
        let mut case = sample_case();
        case.base = base;
        case.head = head;
        case.expected[0].path = "case.toml".into();
        case.expected[0].line = expected_line;
        (repo, case)
    }

    fn initialize_repo(repo: &Path) {
        git_output(repo, &["init", "--quiet"]).unwrap();
        git_output(repo, &["config", "user.name", "Agent Review Bench"]).unwrap();
        git_output(
            repo,
            &["config", "user.email", "agent-review-bench@example.com"],
        )
        .unwrap();
    }

    fn commit_file(repo: &Path, content: &str, message: &str) -> String {
        fs::write(repo.join("case.toml"), content).unwrap();
        git_output(repo, &["add", "case.toml"]).unwrap();
        git_output(repo, &["commit", "--quiet", "-m", message]).unwrap();
        git_output(repo, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string()
    }
}
