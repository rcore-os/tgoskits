---
name: review-open-prs
description: Audit open GitHub pull requests in this tgoskits repository, identify non-self PRs that need the current user's review, track the batch with the available todo tool, then dispatch each eligible PR through review-single-pr with its own complete todo and mandatory local runtime validation for every added or changed StarryOS or ArceOS app. Use when the user asks to review all open PRs, review non-self PRs, re-review PRs updated after their last review, or coordinate per-PR review worktrees/subagents.
---

# Review Open PRs

## Goal

Find open PRs that actually need the current user's attention, then review each eligible PR with `review-single-pr`. This skill is only the multi-PR discovery and dispatch layer; single-PR review standards, validation, inline comments, approval, request-changes, conflict repair, and final submission rules live in `review-single-pr`.

By default, do not re-review every open PR. Review PRs the current user has never reviewed, or PRs whose latest commit is newer than the current user's last submitted review. Include draft PRs unless the user explicitly says to skip drafts.

Respect the global subagent policy: spawn subagents only when the user explicitly asks for subagents, delegation, or parallel agent work. Even when workers are used, the main agent owns the final GitHub review submission unless the user explicitly assigns that authority elsewhere.

## Todo Orchestration Gate

After the eligibility pass identifies the PRs and before dispatching detailed review work, create a user-visible batch todo with one concrete item for each eligible PR plus batch-level final head refresh, review submission verification, reviewer assignment, and cleanup. When a todo or plan tool such as `update_plan` is available, calling it is mandatory: invoke it and wait for success before dispatch, keep using it, and do not merely recommend it or replace it with a prose checklist. Treat an empty successful response as success unless the tool reports an error. Diagnose or retry a failed tool call before falling back. Use a visible Markdown checklist only when no todo tool is available or the tool is confirmed unusable, and report that reason. If the tool exposes only pending, in-progress, and completed states, mark an item completed only after recording its completed, not-applicable, or blocking outcome and evidence.

The batch todo does not replace the PR-specific todo required by `review-single-pr`. Every dispatched review must inspect the current PR scope, read all applicable instructions and references, create its own complete todo before detailed review or validation, append newly discovered scope, and audit every item before and after submission. Keep no more than one batch item in progress at a time unless independent PR reviews are explicitly delegated.

## Eligibility Pass

1. Resolve repository and user identity:
   ```bash
   gh auth status
   gh repo view --json nameWithOwner,defaultBranchRef,url
   gh pr list --state open --limit 100 --json number,title,author,headRefName,headRepositoryOwner,baseRefName,updatedAt,isDraft,url,reviewDecision,mergeStateStatus,maintainerCanModify
   ```
2. Exclude PRs authored by the current GitHub user.
3. For each remaining PR, fetch latest commits, reviews, changed files, and current-head CI/check status:
   ```bash
   gh api "repos/<owner>/<repo>/pulls/<pr>/commits?per_page=100"
   gh api "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
   gh api "repos/<owner>/<repo>/pulls/<pr>/files?per_page=100"
   gh pr checks <pr> --repo <owner>/<repo> --watch=false
   ```
4. Mark a PR eligible when the current user has never reviewed it, or when the PR latest commit timestamp is newer than the current user's last submitted review timestamp. Compare against the latest commit date, not `updatedAt`, because comments, CI, or thread resolution can update a PR without code changes.
5. Treat PRs already reviewed by the current user at the latest commit as excluded unless the user explicitly asks for a fresh pass of already-reviewed PRs.
6. Keep a summary of excluded PRs and the reason: self-authored, already reviewed at latest commit, closed, skipped by user scope, or blocked by a stated constraint.

## Validation Strategy

Before dispatching an eligible PR, build a concrete validation plan from the current head's CI status, changed files, PR body, commits, and touched docs/runbooks. Carry it into the PR-specific todo rather than treating it as a substitute for that todo.

If all relevant CI checks already passed on the current head, do not rerun the same broad local CI-equivalent checks merely to duplicate that evidence. Treat successful CI as coverage evidence for the jobs it actually ran, and spend review time on:

- PR body, README, docs, scripts, and config claims that describe a workflow not executed exactly by CI;
- app, QEMU, rootfs, board-adjacent, tool-wrapper, packaging, symbolizer, or manual runbook flows whose command, architecture, preparation, or success marker differs from CI;
- changed tests/configs that CI skipped because of path filters, matrix conditions, draft/branch restrictions, or expected skip behavior;
- suspicious gaps where CI passed but did not exercise the changed behavior, new architecture, new case discovery, or documented user workflow.

Always follow the per-app hard gate from `review-single-pr`: for every app added, directly changed, or explicitly named in a support claim, verify that the PR body or added/changed documentation gives reproducible environment setup, follow it exactly, and run the real app locally on the current head. Successful CI never removes these todo items. Also run local validation when other CI is failing, missing, stale, suspicious, or skipped for the changed surface. Prefer the narrowest command that checks the uncovered claim instead of a whole-workspace repeat.

Carry this plan into the per-PR review and final report. The report must distinguish:

- `CI covered`: relevant successful check names or workflows that were accepted as remote evidence;
- `app runtime required`: every affected app, documented setup source, required architecture, exact local command, expected readiness check, and observable postcondition;
- `app runtime completed or blocking`: current-head local result for every app; when setup or runtime could not complete, the failure stage, key error, unmet condition, and resulting `REQUEST_CHANGES`;
- `CI-missing validated`: exact documented or PR-body workflow, local/manual command run, architecture or target, and observed postcondition;
- `CI-missing not validated`: exact workflow or claim, why it was not run, and whether that limitation blocks approval;
- `duplicative non-app local checks skipped`: non-app CI-equivalent local commands intentionally skipped because current-head CI already covered them.

## Dispatch

For each eligible PR, invoke `review-single-pr` with a prompt that carries the multi-PR context but leaves review decisions to the single-PR skill:

```text
Use $review-single-pr to review PR #<pr> in <owner>/<repo>.

Context from $review-open-prs:
- This PR is eligible because <never reviewed by current user | latest commit <sha/time> is newer than current user's last review <time>>.
- Draft status: <draft|ready>.
- Merge state: <mergeStateStatus>; maintainer edits: <maintainerCanModify>.
- Scope requested by user: <scope summary>.
- Current-head CI summary: <success/failure/pending/skipped counts, relevant check names, stale/missing/suspicious notes>.
- Validation plan: <CI covered evidence>; <every affected app and its documented environment setup, architecture, exact local runtime command, readiness check, and postcondition>; <other CI-missing PR-body/docs workflows to validate>; <duplicative non-app CI-equivalent local checks to skip>; <commands that still must run because CI is missing/failing/suspicious or review-single-pr requires them>.

Review exactly this PR. After reading every applicable instruction, guideline, and runbook, use the available todo tool to create a complete PR-specific todo before detailed review or validation; use a visible Markdown fallback only when no tool is available or it is confirmed unusable. Follow $review-single-pr for worktree setup, duplicate/superseded fix checks, conflict handling policy, targeted validation, Chinese inline comments, head-SHA freshness checks, and final APPROVE or REQUEST_CHANGES submission. Locally configure and run every affected StarryOS or ArceOS app on the current head even when CI passed; documentation, setup, readiness, or runtime failure requires REQUEST_CHANGES with the exact reason. Audit every todo item before submission and again after reviewer assignment and cleanup.
```

If workers or subagents are explicitly allowed, give each worker exactly one PR and one worktree. Worker prompts must say:

- use `review-single-pr` for the actual review procedure;
- after reading all applicable instructions and references, use the available todo tool to create and maintain a complete PR-specific todo before detailed review; use a visible Markdown fallback only when no tool is available or confirmed unusable;
- perform read-only review plus targeted validation only;
- skip broad non-app local checks that only duplicate already-passing current-head CI, but always follow the documented environment setup and locally run every affected StarryOS or ArceOS app on the current head;
- do not submit GitHub reviews;
- do not push contributor branches unless explicitly assigned conflict-repair work, and then prefer local commit only with final push by the main agent;
- return `APPROVE` or `REQUEST_CHANGES`;
- provide `path`, `line`, `side=RIGHT`, and Chinese inline comment body for each blocking issue;
- include commands run and exact failures;
- report each affected app's setup source, setup commands, readiness result, architecture, runtime command, and postcondition or blocking failure, plus CI-covered evidence, other CI-missing workflows validated, CI-missing workflows not validated with reasons, and non-app CI-equivalent local checks skipped as duplicative;
- identify missing reproduction tests for bug fixes.
- audit every todo item before returning and report completed evidence, concrete not-applicable reasons, blocking results, and unfinished items;
- clean temporary worktrees/files before returning, or report the path and reason when cleanup is unsafe.

Before submitting any worker-derived review, the main agent must refresh the PR head, verify each finding still applies to a current right-side diff line, and follow `review-single-pr` submission rules.

## Conflict Handling

For each conflicted eligible PR, dispatch through `review-single-pr`; it owns the conflict policy, including repairing conflicts after an otherwise-approvable review when maintainer edits are allowed. If the user explicitly asks for conflict handling, say that in the dispatch prompt. The main agent must keep conflict repair separate from ordinary review, and must not force-push contributor branches.

## Final Summary

End with a concise summary of:

- reviewed PRs, decision, and key reason;
- PRs excluded from review and why;
- batch and per-PR todo reconciliation: completed evidence, concrete not-applicable reasons, blocking items, and unfinished items;
- for every affected app: setup source, preparation result, architecture, current-head local runtime command, observed postcondition, or exact blocking reason;
- for each reviewed PR: CI-covered evidence, CI-missing PR-body/docs workflows validated locally/manually, CI-missing workflows not validated and why, and non-app CI-equivalent local checks skipped because current-head CI already passed;
- validation commands that failed, could not be run, or revealed that a documented workflow does not match the PR's claim;
- any PRs left for the author because of conflicts, missing maintainer edit permission, stale heads, CI gaps, or insufficient local/manual evidence for CI-missing flows;
- temporary worktrees/files that could not be cleaned and why.
