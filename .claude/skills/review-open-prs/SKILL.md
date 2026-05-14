---
name: review-open-prs
description: Audit open GitHub pull requests in this tgoskits repository, identify non-self PRs that need the current user's review, then dispatch each eligible PR through review-single-pr. Use when the user asks to review all open PRs, review non-self PRs, re-review PRs updated after their last review, or coordinate per-PR review worktrees/subagents.
---

# Review Open PRs

## Goal

Find open PRs that actually need the current user's attention, then review each eligible PR with `review-single-pr`. This skill is only the multi-PR discovery and dispatch layer; single-PR review standards, validation, inline comments, approval, request-changes, conflict repair, and final submission rules live in `review-single-pr`.

By default, do not re-review every open PR. Review PRs the current user has never reviewed, or PRs whose latest commit is newer than the current user's last submitted review. Include draft PRs unless the user explicitly says to skip drafts.

Respect the global subagent policy: spawn subagents only when the user explicitly asks for subagents, delegation, or parallel agent work. Even when workers are used, the main agent owns the final GitHub review submission unless the user explicitly assigns that authority elsewhere.

## Eligibility Pass

1. Resolve repository and user identity:
   ```bash
   gh auth status
   gh repo view --json nameWithOwner,defaultBranchRef,url
   gh pr list --state open --limit 100 --json number,title,author,headRefName,headRepositoryOwner,baseRefName,updatedAt,isDraft,url,reviewDecision,mergeStateStatus,maintainerCanModify
   ```
2. Exclude PRs authored by the current GitHub user.
3. For each remaining PR, fetch latest commits, reviews, and changed files:
   ```bash
   gh api "repos/<owner>/<repo>/pulls/<pr>/commits?per_page=100"
   gh api "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
   gh api "repos/<owner>/<repo>/pulls/<pr>/files?per_page=100"
   ```
4. Mark a PR eligible when the current user has never reviewed it, or when the PR latest commit timestamp is newer than the current user's last submitted review timestamp. Compare against the latest commit date, not `updatedAt`, because comments, CI, or thread resolution can update a PR without code changes.
5. Treat PRs already reviewed by the current user at the latest commit as excluded unless the user explicitly asks for a fresh pass of already-reviewed PRs.
6. Keep a summary of excluded PRs and the reason: self-authored, already reviewed at latest commit, closed, skipped by user scope, or blocked by a stated constraint.

## Dispatch

For each eligible PR, invoke `review-single-pr` with a prompt that carries the multi-PR context but leaves review decisions to the single-PR skill:

```text
Use $review-single-pr to review PR #<pr> in <owner>/<repo>.

Context from $review-open-prs:
- This PR is eligible because <never reviewed by current user | latest commit <sha/time> is newer than current user's last review <time>>.
- Draft status: <draft|ready>.
- Merge state: <mergeStateStatus>; maintainer edits: <maintainerCanModify>.
- Scope requested by user: <scope summary>.

Review exactly this PR. Follow $review-single-pr for worktree setup, duplicate/superseded fix checks, conflict handling policy, local validation, Chinese inline comments, head-SHA freshness checks, and final APPROVE or REQUEST_CHANGES submission.
```

If workers or subagents are explicitly allowed, give each worker exactly one PR and one worktree. Worker prompts must say:

- use `review-single-pr` for the actual review procedure;
- perform read-only review plus local validation only;
- do not submit GitHub reviews;
- do not push contributor branches unless explicitly assigned conflict-repair work, and then prefer local commit only with final push by the main agent;
- return `APPROVE` or `REQUEST_CHANGES`;
- provide `path`, `line`, `side=RIGHT`, and Chinese inline comment body for each blocking issue;
- include commands run and exact failures;
- identify missing reproduction tests for bug fixes.
- clean temporary worktrees/files before returning, or report the path and reason when cleanup is unsafe.

Before submitting any worker-derived review, the main agent must refresh the PR head, verify each finding still applies to a current right-side diff line, and follow `review-single-pr` submission rules.

## Conflict Handling

For each conflicted eligible PR, dispatch through `review-single-pr`; it owns the conflict policy, including repairing conflicts after an otherwise-approvable review when maintainer edits are allowed. If the user explicitly asks for conflict handling, say that in the dispatch prompt. The main agent must keep conflict repair separate from ordinary review, and must not force-push contributor branches.

## Final Summary

End with a concise summary of:

- reviewed PRs, decision, and key reason;
- PRs excluded from review and why;
- validation commands that failed or could not be run;
- any PRs left for the author because of conflicts, missing maintainer edit permission, stale heads, or insufficient local/CI evidence;
- temporary worktrees/files that could not be cleaned and why.
