---
name: review-open-prs
description: Review eligible open GitHub pull requests in this tgoskits repository. Use when the user asks to audit all PRs, review non-self PRs, re-review PRs updated after their last review, use subagents/worktrees for PR review, compare syscall/network/filesystem behavior with POSIX or Linux semantics, run local verification before approving, or submit GitHub approve/request-changes reviews with Chinese inline comments.
---

# Review Open PRs

## Overview

Review only open PRs that actually need the current user attention, using isolated worktrees and local validation before submitting GitHub reviews. By default this is not a full re-review of every open PR: review PRs the current user has never reviewed, or PRs whose latest commit is newer than the current user's last submitted review. The default outcome is a submitted `APPROVE` review when no blocking issue remains, or a submitted `REQUEST_CHANGES` review with Chinese inline comments when correctness, standards compliance, tests, or CI coverage are insufficient.

Respect the global subagent policy: spawn subagents only when the user explicitly asks for subagents, delegation, or parallel agent work. If subagents are allowed, use them for bounded per-PR review work and keep final GitHub submission in the main agent.

## Eligibility

1. Resolve repository and user identity:
   ```bash
   gh auth status
   gh repo view --json nameWithOwner,defaultBranchRef,url
   gh pr list --state open --limit 100 --json number,title,author,headRefName,headRepositoryOwner,baseRefName,updatedAt,isDraft,url,reviewDecision
   ```
2. Exclude PRs authored by the current GitHub user.
3. For each remaining PR, fetch latest commit and the current user's last review:
   ```bash
   gh api "repos/<owner>/<repo>/pulls/<pr>/commits?per_page=100"
   gh api "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
   gh api "repos/<owner>/<repo>/pulls/<pr>/files?per_page=100"
   ```
4. Mark a PR eligible when the user has never reviewed it, or the PR latest commit time is newer than the user's last review time. Compare the PR latest commit date against the current user's last submitted review timestamp, not `updatedAt`, because comments, CI, or thread resolution can update the PR without new code.
5. Include drafts unless the user explicitly says to skip drafts; note draft status in the summary.
6. For PRs already reviewed by the current user at the latest commit, do not submit another review unless the user explicitly asks for a fresh pass of those PRs. A general request like "review open PRs" still means review only eligible/unreviewed-or-updated PRs, not every open PR. If the user asks to audit already-approved PRs too, treat that as an explicit expanded scope for that turn only.
7. List excluded PRs and the reason: self-authored, already reviewed at latest commit, closed, or skipped by user scope.

## Duplicate or Superseded Fixes

Before approving a PR that fixes a user-visible bug, check whether the same bug is already fixed on the latest base branch or in another open PR:

```bash
git fetch origin <base> '+refs/pull/*/head:refs/remotes/origin/pr/*'
gh pr list --state open --limit 100 --search '<bug keyword or command>'
git grep -n -E '<relevant symbols|paths|commands>' origin/<base> -- <likely paths>
gh pr diff <related-pr> --patch --color=never
```

Use `gh pr diff` or `git diff origin/<base>...origin/pr/<pr>` to understand the PR's own patch. Use `git diff origin/<base>..origin/pr/<pr>` only when intentionally checking how stale the branch is against current base; stale branches can show large unrelated reversions.

Treat a PR as not mergeable when it is superseded by a more complete PR or would regress newer base-branch work. In that case, leave a neutral project-focused comment explaining why the newer PR or base implementation should be preferred. If asked to close such a PR, prefer:

```bash
gh pr comment <pr> --body-file comment.md
gh pr close <pr>
```

Some `gh` versions do not support `gh pr close --comment-file`; avoid shell backticks in inline `--comment` strings because they can be executed by the shell.

## Worktrees

Fetch PR heads and create one isolated worktree per eligible PR:

```bash
git fetch origin '+refs/pull/*/head:refs/remotes/origin/pr/*' '+refs/heads/*:refs/remotes/origin/*'
git worktree add --detach /home/zhourui/opensource/tgoskits-review-pr<pr> origin/pr/<pr>
```

Never review multiple StarryOS QEMU cases in the same checkout at the same time. Use separate worktrees for parallel PR review, and do not modify or revert the user's main worktree.

If a review worktree already exists, verify it is clean and at the expected PR head before reusing it:

```bash
git -C /home/zhourui/opensource/tgoskits-review-pr<pr> status --short
git -C /home/zhourui/opensource/tgoskits-review-pr<pr> rev-parse HEAD
git rev-parse refs/remotes/origin/pr/<pr>
```

If the existing worktree is stale and clean, update it to the fetched PR head with a non-destructive detached checkout. If it has local changes, do not overwrite them; create a fresh worktree path or ask how to proceed.

When spawning workers, give each worker exactly one PR and one worktree. Tell workers to:

- perform read-only review plus local validation only;
- not submit GitHub reviews;
- return `APPROVE` or `REQUEST_CHANGES`;
- provide `path`, `line`, `side=RIGHT`, and Chinese inline comment body for each blocking issue;
- include commands run and exact failures;
- identify missing reproduction tests for bug fixes.

## Review Threads

Use thread-aware review data whenever the task includes resolving old comments or deciding whether previous requested changes are fixed. Flat review comments are not enough because they omit `isResolved`, `isOutdated`, and thread IDs.

Fetch review threads with GraphQL:

```bash
gh api graphql -F query=@query.graphql -F owner=<owner> -F repo=<repo> -F number=<pr>
```

The query must include `reviewThreads { nodes { id isResolved isOutdated path line diffSide comments(first: 100) { nodes { author { login } body createdAt } } } }`. Detached worktrees cannot rely on `gh pr view` branch inference, so pass `<owner>`, `<repo>`, and `<pr>` explicitly when using helper scripts.

When resolving threads:

- resolve only threads whose concrete issue is fixed in the current PR head;
- keep threads open when the fix is partial, the test is not wired into the runner, or the comment is still behaviorally valid;
- resolving an old thread does not imply approval if new blocking issues remain;
- after resolving, fetch threads again and confirm `isResolved=true`.

Resolve with:

```bash
gh api graphql \
  -f query='mutation($threadId:ID!){resolveReviewThread(input:{threadId:$threadId}){thread{id isResolved}}}' \
  -f threadId=<thread-id>
```

## Review Standards

Review code against the PR's stated intent, existing project patterns, and relevant external semantics:

- POSIX/Linux semantics for syscalls, filesystem errors, process/session/signal behavior, sockets, IPv4/IPv6, `IPV6_V6ONLY`, and `/proc`.
- RFCs or Linux behavior for networking details such as IPv6 NDP, IPv4-mapped IPv6, dual-stack listeners, route/listen conflicts, and errno behavior.
- VirtIO, PCI, DMA, MMIO, IRQ, and driver ownership rules for driver changes.
- Axvisor VM config semantics for `entry_point`, `kernel_load_addr`, `memory_regions`, `map_type`, and guest image layout.
- StarryOS test-suit layout rules from `starry-test-suit` when test cases or `qemu-*.toml` files change.
- `cross-kernel-driver` architecture rules when portable driver crates or driver glue change.

For bug fixes, require a reproduction test that fails before the fix and passes after it, unless the environment makes that impossible. If a reproduction cannot be run locally, explain the blocker and what evidence was checked instead.

## Validation

Always run local verification that matches the changed surface. Prefer project `xtask` commands:

- Baseline formatting:
  ```bash
  cargo fmt --check
  ```
- Changed Rust crate:
  ```bash
  cargo xtask clippy --package <crate>
  ```
- Crates outside the workspace or special manifests:
  ```bash
  cargo clippy --manifest-path <path>/Cargo.toml --all-features -- -D warnings
  ```
- StarryOS cases:
  ```bash
  cargo xtask starry test qemu --arch <arch> -c <case>
  ```
- Axvisor configs:
  ```bash
  cargo xtask axvisor build ... --vmconfigs <config>
  ```

If `cargo xtask` cannot satisfy a special configuration, inspect the relevant `xtask` help or source first, then fall back to a native Cargo command with matched arguments. Record exact command output for failures such as unknown package names, QEMU timeout, missing guest image, or clippy diagnostics.

For StarryOS grouped QEMU cases, verify that newly listed commands are actually installed into the guest overlay. A `qemu-*.toml` `test_commands` entry such as `/usr/bin/<test>` must correspond to a case/subcase asset path that the runner discovers and builds. Running the containing grouped case is the preferred check, for example:

```bash
cargo xtask starry test qemu --arch x86_64 -c syscall
```

Treat `/usr/bin/<test>: not found`, `status=127`, skipped discovery, or an unbuilt asset directory as blocking even when the Rust code and clippy pass.

Use GitHub check status as required evidence, but not as the only review input:

```bash
gh pr checks <pr> --watch=false
```

Do not approve solely because remote CI passes; local review and targeted validation still matter. Conversely, if required checks are failing, cancelled, or missing for a branch that needs CI coverage, treat that as blocking unless there is a clear project-approved reason. A branch with "no checks reported" is not equivalent to passing; require targeted local validation before approving, and request changes when the changed surface is too large or risky to validate locally.

## Findings

Treat these as blocking unless clearly non-blocking:

- behavior differs from POSIX/Linux/RFC/VirtIO semantics;
- local targeted tests or clippy fail;
- new tests are not discovered by the project test runner;
- `success_regex` or `fail_regex` cannot reliably classify the intended StarryOS case result;
- bug fix lacks a meaningful reproduction test;
- submitted buffers, DMA memory, queue tokens, or IRQ ownership can be leaked, freed too early, or handled in the wrong layer;
- a change silently makes CI hang, time out, or skip the new coverage.
- an older PR duplicates or weakens a fix already present on the base branch or in a more complete open PR.

Inline comments must be in Chinese, neutral, and project-focused. Each comment should include:

1. the concrete problem;
2. the relevant standard, project rule, or observed test failure;
3. a suggested fix.

Prefer commenting on changed lines in the PR diff. If GitHub cannot resolve a comment line, move the comment to the nearest changed line that demonstrates the problem or put the finding in the review body. Before submitting inline comments from a worker, verify each `line` is present on the right side of the current PR diff; context or unchanged lines may be rejected by the review API.

## Submit Reviews

Before submission, confirm the PR head SHA has not changed:

```bash
gh pr view <pr> --json number,headRefOid,reviewDecision
```

Submit with the GitHub review API so inline comments and final event land together:

```bash
gh api --method POST repos/<owner>/<repo>/pulls/<pr>/reviews --input review.json
```

Use `REQUEST_CHANGES` when there is any blocking issue. Use `APPROVE` when there are no blocking issues; non-blocking suggestions may be included as comments or in the review body.

Inline review payloads must include the current `headRefOid` as `commit_id`, and each inline comment should use a changed-line anchor on `side=RIGHT`:

```json
{
  "commit_id": "<headRefOid>",
  "event": "REQUEST_CHANGES",
  "body": "...",
  "comments": [
    {"path": "path/to/file.rs", "line": 123, "side": "RIGHT", "body": "..."}
  ]
}
```

If a worker returns a finding on a line that is not present on the current PR diff, move the comment to the nearest changed line that demonstrates the problem or put the finding in the review body.

Review body should summarize:

- decision;
- local validation commands and results;
- for failing tests, the exact failure mode;
- for bug fixes, reproduction coverage status;
- any known environment limitation.

After submission, verify final state:

```bash
gh pr view <pr> --json number,reviewDecision,latestReviews
```

End with a concise user summary listing each reviewed PR, decision, key reason, and review link.
