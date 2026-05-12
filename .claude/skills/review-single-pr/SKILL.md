---
name: review-single-pr
description: Review one specified GitHub pull request in this tgoskits repository. Use when the user names a PR number or URL and asks to review, re-review, compare with Linux/POSIX/RFC/VirtIO semantics, run focused validation, leave Chinese inline review comments, approve, or request changes.
---

# Review Single PR

## Goal

Perform a focused review of exactly one PR, using an isolated worktree and local validation before submitting a GitHub review. The normal outcome is either `APPROVE` when no blocking issue remains, or `REQUEST_CHANGES` with Chinese inline comments when the PR has correctness, standards, test, or CI coverage problems.

This skill is the single-PR subset of `review-open-prs`: do not scan all open PRs unless needed to check duplicate or superseded fixes.

## System Skill Priority

For GitHub operations, follow the system GitHub plugin skills first:

- Use `github:github` as the default source for repository orientation, PR metadata, patch inspection, comments, labels, reactions, and connector-first behavior.
- Use `github:gh-address-comments` when unresolved review threads, requested changes, inline review context, line anchors, or thread resolution state matter.
- Use `github:gh-fix-ci` when the review depends on failing GitHub Actions checks or logs.

Prefer the GitHub MCP/connector for structured PR data. Use local `git` for fetch, detached worktrees, local diffs, and validation. Use `gh` only for connector gaps such as current-branch PR discovery, GraphQL review-thread state, Actions logs, or review submission when the connector cannot preserve the required inline review anchors.

## Intake

1. Follow `github:github` to resolve repository identity, current user, PR number or URL, title, author, base/head refs, `headRefOid`, draft state, merge state, changed files, patch context, existing reviews/comments, and available checks.
2. If the PR is authored by the current GitHub user, say so and ask before submitting a formal review.
3. Include draft PRs unless the user explicitly says to skip drafts.
4. Keep connector state and local checkout state aligned before creating the worktree.

Fallback only when the GitHub MCP/connector cannot provide the needed data:

   ```bash
   gh auth status
   gh repo view --json nameWithOwner,defaultBranchRef,url
   gh pr view <pr> --json number,title,author,baseRefName,headRefName,headRefOid,headRepositoryOwner,isDraft,mergeStateStatus,maintainerCanModify,reviewDecision,url
   gh pr diff <pr> --patch --color=never
   gh pr checks <pr> --watch=false
   gh api "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
   gh api "repos/<owner>/<repo>/pulls/<pr>/files?per_page=100"
   ```

## Review Threads And CI

For prior requested changes, unresolved review threads, inline review locations, or resolution state, follow `github:gh-address-comments`: use the GitHub app/MCP for PR metadata and patch context, and use its GraphQL-based `gh` fallback only when thread-level fields such as `isResolved`, `isOutdated`, `diffSide`, or exact line anchors are required. Do not treat flat connector comments as a complete representation of review-thread state.

For failing or suspicious GitHub Actions checks, follow `github:gh-fix-ci`: use the GitHub app/MCP for PR context and use `gh` for Actions check/log inspection because the connector does not expose that workflow end to end. Remote CI is evidence, not a substitute for local review and targeted validation.

## Worktree

Fetch the PR and base, then review in a detached worktree:

```bash
repo_root="$(git rev-parse --show-toplevel)"
repo_parent="$(dirname "$repo_root")"
review_wt="$repo_parent/$(basename "$repo_root")-review-pr<pr>"
git fetch origin '+refs/pull/<pr>/head:refs/remotes/origin/pr/<pr>' '+refs/heads/*:refs/remotes/origin/*'
git worktree add --detach "$review_wt" origin/pr/<pr>
```

If the worktree already exists, reuse it only when clean and at the current PR head:

```bash
git -C "$review_wt" status --short
git -C "$review_wt" rev-parse HEAD
git rev-parse refs/remotes/origin/pr/<pr>
```

If it is stale and clean, update it non-destructively to the fetched PR head. If it has local changes, create a fresh worktree path or ask how to proceed. Do not modify or revert the user's main worktree while reviewing.

## Review Focus

Review the PR against its stated intent, the current base branch, existing project patterns, and relevant external semantics:

- POSIX/Linux behavior for syscalls, process/session/signal semantics, filesystem errors, sockets, IPv4/IPv6, and `/proc`.
- RFC or Linux behavior for networking details such as IPv6 NDP, IPv4-mapped IPv6, dual-stack listeners, route/listen conflicts, and errno behavior.
- VirtIO, PCI, DMA, MMIO, IRQ, and ownership rules for driver changes.
- Axvisor config semantics for `entry_point`, `kernel_load_addr`, `memory_regions`, `map_type`, and guest image layout.
- `starry-test-suit` rules when StarryOS test cases or `qemu-*.toml` files change.
- `cross-kernel-driver` architecture rules when portable driver crates or driver glue change.

For bug fixes, require a reproduction test that fails before the fix and passes after it unless the environment makes that impossible. For raw syscall fixes, prefer direct `syscall(SYS_...)` coverage when libc wrappers could mask return values or errno.

Before approving a bugfix PR, check whether the same bug is already fixed on base or in another open PR:

```bash
git grep -n -E '<relevant symbols|paths|commands>' origin/<base> -- <likely paths>
```

Use the GitHub MCP/connector to search or list related open PRs first. Fallback only when the connector cannot search the needed PR set:

```bash
gh pr list --state open --limit 100 --search '<bug keyword or command>'
```

Use `git diff origin/<base>...origin/pr/<pr>` for the PR patch. Use `origin/<base>..origin/pr/<pr>` only when intentionally checking stale-branch effects.

## Validation

Run focused validation matching the changed surface. Prefer project `xtask` commands:

```bash
cargo fmt --check
cargo xtask clippy --package <crate>
cargo xtask starry test qemu --arch <arch> -c <case>
cargo xtask axvisor build ... --vmconfigs <config>
```

If `cargo xtask` does not cover a special configuration, inspect the relevant `xtask` help or source before falling back to native Cargo with matched arguments. Record exact commands and failures.

For StarryOS grouped QEMU cases, verify that new `test_commands` are actually discovered and installed into the guest overlay. Treat `/usr/bin/<test>: not found`, `status=127`, skipped discovery, unbuilt asset directories, unreliable `success_regex`/`fail_regex`, or tests that accept both broken and fixed behavior as blocking.

Remote CI is useful evidence but not a substitute for local review. A branch with no reported checks is not equivalent to passing.

## Blocking Findings

Treat these as blocking unless clearly non-blocking:

- behavior differs from POSIX/Linux/RFC/VirtIO semantics;
- targeted tests, formatting, clippy, or CI fail;
- new tests are not discovered or do not exercise the fixed ABI surface;
- bug fixes lack meaningful reproduction coverage;
- submitted buffers, DMA memory, queue tokens, or IRQ ownership can leak, be freed too early, or cross the wrong abstraction layer;
- the PR duplicates, weakens, or is superseded by a newer base-branch or open-PR fix.

Inline comments must be Chinese, neutral, and project-focused. Each comment should include the concrete problem, the relevant standard/project rule/observed failure, and a suggested fix.

Prefer changed lines on the PR diff. Before submitting, verify every inline `line` exists on the current right side of the diff; if GitHub cannot resolve a line, move to the nearest changed line that demonstrates the issue or put the finding in the review body.

## Submit Review

Before submitting, confirm through the GitHub MCP/connector that the PR head SHA has not changed. Fallback only when connector data is unavailable:

```bash
gh pr view <pr> --json number,headRefOid,reviewDecision
```

If the head changed after analysis or validation, fetch the new head, update the worktree, re-check each finding on current changed lines, and rerun the targeted validation that supports the decision.

Submit the final review through the GitHub MCP/connector when it can send the review event and inline comments with preserved anchors together. If the connector cannot submit inline review comments or cannot preserve line anchors, fallback to the GitHub review REST API via `gh`:

```bash
gh api --method POST repos/<owner>/<repo>/pulls/<pr>/reviews --input review.json
```

Use the current `headRefOid` as `commit_id`, `side=RIGHT` for inline comments, `REQUEST_CHANGES` for any blocking issue, and `APPROVE` only when no blocking issue remains:

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

Do not submit stale findings against an old head.
