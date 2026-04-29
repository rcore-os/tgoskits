---
name: review-open-prs
description: Review eligible open GitHub pull requests in this tgoskits repository. Use when the user asks to audit all PRs, review non-self PRs, re-review PRs updated after their last review, use subagents/worktrees for PR review, compare syscall/network/filesystem behavior with POSIX or Linux semantics, run local verification before approving, or submit GitHub approve/request-changes reviews with Chinese inline comments.
---

# Review Open PRs

## Overview

Review all open PRs that actually need the current user attention, using isolated worktrees and local validation before submitting GitHub reviews. The default outcome is a submitted `APPROVE` review when no blocking issue remains, or a submitted `REQUEST_CHANGES` review with Chinese inline comments when correctness, standards compliance, tests, or CI coverage are insufficient.

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
4. Mark a PR eligible when the user has never reviewed it, or the PR latest commit time is newer than the user's last review time.
5. Include drafts unless the user explicitly says to skip drafts; note draft status in the summary.
6. List excluded PRs and the reason: self-authored, already reviewed at latest commit, closed, or skipped by user scope.

## Worktrees

Fetch PR heads and create one isolated worktree per eligible PR:

```bash
git fetch origin '+refs/pull/*/head:refs/remotes/origin/pr/*' '+refs/heads/*:refs/remotes/origin/*'
git worktree add --detach /home/zhourui/opensource/tgoskits-review-pr<pr> origin/pr/<pr>
```

Never review multiple StarryOS QEMU cases in the same checkout at the same time. Use separate worktrees for parallel PR review, and do not modify or revert the user's main worktree.

When spawning workers, give each worker exactly one PR and one worktree. Tell workers to:

- perform read-only review plus local validation only;
- not submit GitHub reviews;
- return `APPROVE` or `REQUEST_CHANGES`;
- provide `path`, `line`, `side=RIGHT`, and Chinese inline comment body for each blocking issue;
- include commands run and exact failures;
- identify missing reproduction tests for bug fixes.

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

Use GitHub check status only as auxiliary evidence:

```bash
gh pr checks <pr> --watch=false
```

Do not approve solely because remote CI passes; local review and targeted validation still matter.

## Findings

Treat these as blocking unless clearly non-blocking:

- behavior differs from POSIX/Linux/RFC/VirtIO semantics;
- local targeted tests or clippy fail;
- new tests are not discovered by the project test runner;
- `success_regex` or `fail_regex` cannot reliably classify the intended StarryOS case result;
- bug fix lacks a meaningful reproduction test;
- submitted buffers, DMA memory, queue tokens, or IRQ ownership can be leaked, freed too early, or handled in the wrong layer;
- a change silently makes CI hang, time out, or skip the new coverage.

Inline comments must be in Chinese, neutral, and project-focused. Each comment should include:

1. the concrete problem;
2. the relevant standard, project rule, or observed test failure;
3. a suggested fix.

Prefer commenting on changed lines in the PR diff. If GitHub cannot resolve a comment line, move the comment to the nearest changed line or put it in the review body.

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
