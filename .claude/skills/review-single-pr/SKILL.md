---
name: review-single-pr
description: Review one specified GitHub pull request in this tgoskits repository. Use when the user names a PR number or URL and asks to review, re-review, compare with Linux/POSIX/RFC/VirtIO semantics, run focused validation, leave Chinese inline review comments, approve, or request changes.
---

# Review Single PR

## Goal

Perform a focused review of exactly one PR, using an isolated worktree and local validation before submitting a GitHub review. The normal outcome is either `APPROVE` when no blocking issue remains, or `REQUEST_CHANGES` with Chinese inline comments when the PR has correctness, standards, test, or CI coverage problems.

This skill is the authoritative single-PR workflow used by `review-open-prs`: do not scan all open PRs unless needed to check duplicate or superseded fixes.

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

For prior requested changes, unresolved review conversations, inline review locations, or resolution state, follow `github:gh-address-comments`: use the GitHub app/MCP for PR metadata and patch context, and use its GraphQL-based `gh` fallback only when thread-level fields such as `isResolved`, `isOutdated`, `diffSide`, or exact line anchors are required. Do not treat flat connector comments as a complete representation of review-thread state.

When using GraphQL directly, request `reviewThreads { nodes { id isResolved isOutdated path line diffSide comments(first: 100) { nodes { author { login } body createdAt } } } }`. Detached worktrees cannot rely on current-branch PR inference, so pass `<owner>`, `<repo>`, and `<pr>` explicitly to helpers.

Always inspect unresolved review conversations from previous reviews. If the concrete issue is fixed in the current PR head, resolve the conversation before finishing the review. Keep threads open when the fix is partial, the test is not wired into the runner, or the comment is still behaviorally valid. Resolving old threads does not imply approval if new blocking issues remain.

Resolve fixed conversations with the review-thread API, then fetch threads again and confirm every resolved thread reports `isResolved=true`:

```bash
gh api graphql \
  -f query='mutation($threadId:ID!){resolveReviewThread(input:{threadId:$threadId}){thread{id isResolved}}}' \
  -f threadId=<thread-id>
```

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

Never review multiple StarryOS QEMU cases in the same checkout at the same time. Use separate worktrees for parallel PR review.

## Merge Conflicts

Handle merge conflicts in either of these cases: the user explicitly asks for conflict handling, or the review has no blocking findings and would otherwise be `APPROVE` while `mergeStateStatus=DIRTY` and `maintainerCanModify=true`. Do not submit approval before the conflict repair is pushed and re-validated against the new head.

- If `mergeStateStatus` is `DIRTY` and `maintainerCanModify=false`, do not repair the branch. When conflict handling was explicitly requested, submit `REQUEST_CHANGES` explaining that the branch conflicts with base and maintainers cannot push a fix; ask the author to merge/rebase latest base and suggest enabling "Allow edits by maintainers". Otherwise, include the conflict limitation in the review body or user summary.
- If `mergeStateStatus` is `DIRTY` and `maintainerCanModify=true`, create a separate conflict worktree, check out `origin/pr/<pr>` detached, merge `origin/<base>`, resolve conflicts according to PR intent and current base behavior, validate, then commit the merge locally.
- Before pushing a repaired conflict branch, refresh the PR and confirm the local merge commit's first parent equals the current remote `headRefOid`. If the remote head changed, stop and rebase/re-review instead of pushing.
- Push repaired fork branches with a normal non-force push to the PR head owner and branch, for example `git push https://github.com/<head-owner>/<repo>.git HEAD:<headRefName>`. Never force-push a contributor branch.
- After pushing conflict repairs, refresh PR status, update the review worktree to the new head, and rerun the targeted validation that supports approval. Submit `APPROVE` only if the repaired head still has no blocking findings; otherwise submit `REQUEST_CHANGES` with the remaining conflict or validation problem.
- `BLOCKED` or `UNSTABLE` may remain because of CI or reviews even after conflicts are gone; do not treat that alone as failed conflict repair.

## Review Focus

Review the PR against its stated intent, the current base branch, existing project patterns, and relevant external semantics. Understand the implementation logic, not just whether tests pass:

- POSIX/Linux behavior for syscalls, process/session/signal semantics, filesystem errors, sockets, IPv4/IPv6, and `/proc`.
- RFC or Linux behavior for networking details such as IPv6 NDP, IPv4-mapped IPv6, dual-stack listeners, route/listen conflicts, and errno behavior.
- VirtIO, PCI, DMA, MMIO, IRQ, and ownership rules for driver changes.
- Axvisor config semantics for `entry_point`, `kernel_load_addr`, `memory_regions`, `map_type`, and guest image layout.
- `starry-test-suit` rules when StarryOS test cases or `qemu-*.toml` files change.
- `cross-kernel-driver` architecture rules when portable driver crates or driver glue change.

For bug fixes, require a reproduction test that fails before the fix and passes after it unless the environment makes that impossible. For raw syscall fixes, prefer direct `syscall(SYS_...)` coverage when libc wrappers could mask return values or errno.

Do not approve changes that are only shaped to satisfy the added tests, such as hard-coded special cases, skipped behavior, fake state updates, no-op compatibility shims, or logic that does not implement the intended subsystem semantics. Treat this as blocking even when local tests and CI pass.

Do not accept "success path" tests that silently skip on unexpected failure, such as returning early when `brk`, `sbrk`, I/O, or socket setup returns `ENOMEM`/`EAGAIN`, unless the test prints an explicit skip marker and the review explains why the environment legitimately cannot require success. Bugfix reproduction tests should fail loudly when the fixed behavior is absent.

Before approving a bugfix PR, check whether the same bug is already fixed on base or in another open PR:

```bash
git grep -n -E '<relevant symbols|paths|commands>' origin/<base> -- <likely paths>
```

Use the GitHub MCP/connector to search or list related open PRs first. Fallback only when the connector cannot search the needed PR set:

```bash
gh pr list --state open --limit 100 --search '<bug keyword or command>'
gh pr diff <related-pr> --patch --color=never
```

Use `git diff origin/<base>...origin/pr/<pr>` for the PR patch. Use `origin/<base>..origin/pr/<pr>` only when intentionally checking stale-branch effects.

Treat a PR as not mergeable when it is superseded by a more complete PR or would regress newer base-branch work. Leave a neutral project-focused comment explaining why the newer PR or base implementation should be preferred. If asked to close such a PR, prefer `gh pr comment <pr> --body-file comment.md` followed by `gh pr close <pr>`; avoid shell backticks in inline `--comment` strings.

## Validation

Run focused validation matching the changed surface. Prefer project `xtask` commands:

```bash
cargo fmt --check
cargo xtask clippy --package <crate>
cargo clippy --manifest-path <path>/Cargo.toml --all-features -- -D warnings
cargo xtask starry test qemu --arch <arch> -c <case>
cargo xtask axvisor build ... --vmconfigs <config>
```

If `cargo xtask` does not cover a special configuration, inspect the relevant `xtask` help or source before falling back to native Cargo with matched arguments. Record exact commands and failures.

For StarryOS grouped QEMU cases, verify that new `test_commands` are actually discovered and installed into the guest overlay. A `qemu-*.toml` command such as `/usr/bin/<test>` must correspond to a case/subcase asset path that the runner discovers and builds. Running the containing grouped case is the preferred check, for example `cargo xtask starry test qemu --arch x86_64 -c syscall`. Treat `/usr/bin/<test>: not found`, `status=127`, skipped discovery, unbuilt asset directories, unreliable `success_regex`/`fail_regex`, or tests that accept both broken and fixed behavior as blocking.

For bugfix tests in grouped cases, inspect the new test's assertions as well as running the case. A grouped case passing is not sufficient when the new test accepts both the fixed behavior and the broken behavior.

Use GitHub check status as required evidence, but not as the only review input:

```bash
gh pr checks <pr> --watch=false
```

Do not approve solely because remote CI passes. Conversely, if required checks are failing, cancelled, or missing for a branch that needs CI coverage, treat that as blocking unless there is a clear project-approved reason. A branch with no reported checks is not equivalent to passing; require targeted local validation before approving, and request changes when the changed surface is too large or risky to validate locally.

## Blocking Findings

Treat these as blocking unless clearly non-blocking:

- behavior differs from POSIX/Linux/RFC/VirtIO semantics;
- targeted tests, formatting, clippy, or CI fail;
- new tests are not discovered by the project test runner or do not exercise the fixed ABI surface;
- `success_regex` or `fail_regex` cannot reliably classify the intended StarryOS case result;
- bug fixes lack meaningful reproduction coverage;
- the implementation is a test-only or fake fix that does not implement the intended behavior;
- submitted buffers, DMA memory, queue tokens, or IRQ ownership can leak, be freed too early, or cross the wrong abstraction layer;
- a change silently makes CI hang, time out, or skip the new coverage;
- the PR duplicates, weakens, or is superseded by a newer base-branch or open-PR fix.

All GitHub review text, including inline comments, review body, and replies, must be in Chinese, neutral, and project-focused. Each blocking comment should include the concrete problem, the relevant standard/project rule/observed failure, and a suggested fix.

Prefer changed lines on the PR diff. Before submitting, verify every inline `line` exists on the current right side of the diff; if GitHub cannot resolve a line, move to the nearest changed line that demonstrates the issue or put the finding in the review body. Context or unchanged lines may be rejected by the review API.

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

If a worker returns a finding on a line that is not present on the current PR diff, move the comment to the nearest changed line that demonstrates the problem or put the finding in the review body.

After submission, re-query the PR. If a new commit landed during review submission, refresh the worktree and submit a follow-up review only if the blocking issue still applies to the new head.

Review body must explain in Chinese:

- what the PR changed;
- the implementation logic and why this approach is correct for the project semantics;
- validation commands and results, including exact failure mode for failing tests;
- reproduction coverage status for bug fixes;
- unresolved review conversations that were resolved, and conversations intentionally left open and why;
- any behavior that remains unimplemented, partial, or should be completed in future work;
- any known environment limitation.

Do not approve when the review cannot explain the implementation logic beyond "tests pass".

Verify final state:

```bash
gh pr view <pr> --json number,reviewDecision,latestReviews
```

## Cleanup

After review submission or an explicit no-submit stop, clean temporary resources before ending:

- Remove clean review and conflict worktrees with `git worktree remove <path>`, then run `git worktree prune` from the main repository.
- Delete temporary files created for review payloads, GraphQL queries, comments, logs, or conflict notes unless the user asked to keep them.
- Do not remove a worktree that has uncommitted conflict-repair work, diagnostics needed for a reported failure, or user-created changes; report the path and reason instead.
- Confirm the main worktree status was not changed by the review workflow.
