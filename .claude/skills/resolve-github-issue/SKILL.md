---
name: resolve-github-issue
description: Resolve recent or specified GitHub issues in this tgoskits repository. Use this skill when the user asks to inspect the latest issue, analyze root cause, avoid loosening tests, use subagents for investigation or review, add deterministic regression coverage, submit a PR, or link a PR so merging closes an issue.
---

# Resolve GitHub Issue

Use this workflow to turn a GitHub issue into a root-cause fix, verified regression test, and project-ready PR. Do not treat CI symptoms as the fix target until the failing path is understood.

## Intake

1. Identify the issue from the user request:
   - If a number or URL is provided, read that issue.
   - If the user says "recent" or "latest", list recent open issues in `rcore-os/tgoskits` and choose the newest relevant one.
2. Read the full issue body, comments, linked PR/check logs, failure snippets, labels, and dates. Record exact failing command, architecture, target, test name, and observed error.
3. Inspect the failing test or workflow before changing it. Determine what behavior it is supposed to prove, which runner discovers it, and what success/failure markers mean.
4. If the issue touches Starry or ArceOS test-suit files, also use the matching project skill such as `starry-test-suit` or `arceos-test-adapter`.

## Root Cause First

- Use subagents when the user asks for them or when independent investigation will shorten the path. Good splits are:
  - test semantics and runner behavior;
  - kernel/library call path and direct error source;
  - external reference semantics such as Linux, POSIX, RFC, or VirtIO;
  - review of the final patch.
- Trace from the user-visible error back to the first internal source that produces it. Then trace one layer further to explain why that source is reached.
- Do not fix by only loosening regexes, increasing timeouts, retrying in the test, ignoring `EINTR`, or weakening assertions unless the root cause proves the original expectation was wrong.
- Preserve adjacent contracts while fixing. Examples include nonblocking I/O returning `WouldBlock`, poller/waker registration for epoll readiness, syscall restart semantics, and Starry grouped-test failure propagation.

## Regression Test

1. Add the narrowest deterministic test at the root-cause layer whenever possible. Prefer a unit or component test that directly constructs the bad state over an integration test that depends on timing.
2. Prove the test is meaningful with a red/green check:
   - Temporarily restore or simulate the old behavior.
   - Run the narrow test and confirm it fails for the expected reason.
   - Restore the fix and confirm the same test passes.
3. Keep the original issue-level command as an end-to-end validation when practical, especially for Starry/ArceOS QEMU issues.
4. If a deterministic root-cause test is impossible, document why and add the least flaky integration coverage that still fails loudly on the bug.

## Implementation And Validation

- Make the smallest code change that addresses the root cause. Avoid unrelated refactors.
- Run `cargo fmt` after code edits.
- After modifying a crate, run targeted clippy, preferably:
  ```bash
  cargo xtask clippy --package <crate>
  ```
- Run the narrow regression test and the original issue reproduction command. For Starry, ArceOS, and Axvisor builds/tests/runs, prefer `cargo xtask`.
- Check `git diff --check` before publishing.

## PR Flow

1. Create or use a focused branch for the issue. Keep unrelated changes out.
2. Commit with a Conventional Commits title, usually `fix(<crate-or-area>): <summary>`.
3. Open a draft PR unless the user explicitly asks for ready-for-review.
4. PR title must be English. PR body must be Chinese and cover:
   - the problem and root cause;
   - the change and why each step is needed;
   - deterministic red/green regression evidence;
   - local validation commands and results;
   - `Fixes #<issue>` so merging the PR closes the issue.
5. After adding or changing commits on a PR branch, update the PR description so it stays synchronized with the branch.

## Final Report

Report the issue number, root cause, changed behavior, regression test evidence, validation commands, branch/commit, PR URL, and whether `Fixes #<issue>` is present. If any validation was skipped, state the concrete reason.
