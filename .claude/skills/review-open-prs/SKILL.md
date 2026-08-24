---
name: review-open-prs
description: Audit open GitHub pull requests in this tgoskits repository, identify non-self PRs that need the current user's review, defer PRs whose relevant current-head CI is still running, reuse successful exact CI coverage instead of duplicate local validation, track the reviewable batch with the available todo tool, then dispatch each eligible PR through review-single-pr. Use when the user asks to review all open PRs, review non-self PRs, re-review PRs updated after their last review, or coordinate per-PR review worktrees/subagents.
---

# Review Open PRs

## Goal

Find open PRs that actually need the current user's attention, then review each eligible PR with `review-single-pr`. This skill is only the multi-PR discovery and dispatch layer; single-PR review standards, validation, inline comments, approval, request-changes, conflict repair, and final submission rules live in `review-single-pr`.

By default, do not re-review every open PR. Review PRs the current user has never reviewed, or PRs whose latest commit is newer than the current user's last submitted review. Include draft PRs unless the user explicitly says to skip drafts.

Respect the global subagent policy: spawn subagents only when the user explicitly asks for subagents, delegation, or parallel agent work. Even when workers are used, the main agent owns the final GitHub review submission unless the user explicitly assigns that authority elsewhere.

## Todo Orchestration Gate

After the eligibility and CI-readiness pass identifies reviewable PRs, create a user-visible batch todo with one concrete item for each eligible PR plus batch-level final head refresh, review submission verification, reviewer assignment, and cleanup. Do not create a PR-specific review item for a CI-deferred PR; keep it only in the read-only deferred summary. When a todo or plan tool such as `update_plan` is available, calling it is mandatory: invoke it and wait for success before dispatch, keep using it, and do not merely recommend it or replace it with a prose checklist. Treat an empty successful response as success unless the tool reports an error. Diagnose or retry a failed tool call before falling back. Use a visible Markdown checklist only when no todo tool is available or the tool is confirmed unusable, and report that reason. If the tool exposes only pending, in-progress, and completed states, mark an item completed only after recording its completed, not-applicable, or blocking outcome and evidence.

The batch todo does not replace the PR-specific todo required by `review-single-pr`. Every dispatched review must inspect the current PR scope, read all applicable instructions and references, create its own complete todo before detailed review or validation, append newly discovered scope, and audit every item before and after submission. Keep no more than one batch item in progress at a time unless independent PR reviews are explicitly delegated.

## Eligibility Pass

1. Resolve repository and user identity:
   ```bash
   gh auth status
   gh repo view --json nameWithOwner,defaultBranchRef,url
   gh pr list --state open --limit 100 --json number,title,author,headRefName,headRepositoryOwner,baseRefName,updatedAt,isDraft,url,reviewDecision,mergeStateStatus,maintainerCanModify
   ```
2. Exclude PRs authored by the current GitHub user.
3. For each remaining PR, fetch latest commits, reviews, changed files, and exact-current-head CI/check status:
   ```bash
   gh api "repos/<owner>/<repo>/pulls/<pr>/commits?per_page=100"
   gh api "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
   gh api "repos/<owner>/<repo>/pulls/<pr>/files?per_page=100"
   gh pr checks <pr> --repo <owner>/<repo> --watch=false
   gh api --paginate "repos/<owner>/<repo>/commits/<head-sha>/check-runs?per_page=100"
   gh api --paginate "repos/<owner>/<repo>/actions/runs?head_sha=<head-sha>&per_page=100"
   gh api --paginate "repos/<owner>/<repo>/actions/runs/<run-id>/jobs?per_page=100"
   ```
4. Mark a PR eligible when the current user has never reviewed it, or when the PR latest commit timestamp is newer than the current user's last submitted review timestamp. Compare against the latest commit date, not `updatedAt`, because comments, CI, or thread resolution can update a PR without code changes.
5. For each otherwise-eligible PR, map changed surfaces and claims to relevant checks and jobs. If any relevant check or job is `queued`, `pending`, `waiting`, or `in_progress`, mark the PR `CI deferred`, exclude it from dispatch, and stop processing it. Do not wait, poll, create a PR-specific todo or worktree, run local validation, inspect code in detail, resolve discussions, submit comments or reviews, create or update issues, repair conflicts, or change reviewers. Record only the PR number, exact head SHA, and unfinished relevant checks for the final summary. Ignore an unfinished job only with a concrete changed-scope reason showing it is unrelated.
6. Treat PRs already reviewed by the current user at the latest commit as excluded unless the user explicitly asks for a fresh pass of already-reviewed PRs.
7. Keep separate summaries of excluded PRs and CI-deferred PRs. Exclusion reasons include self-authored, already reviewed at latest commit, closed, skipped by user scope, or blocked by a stated constraint.

## Validation Strategy

After a PR passes the CI-readiness gate and before dispatch, build a CI coverage ledger and concrete validation plan from the exact current head's checks and jobs, changed files, PR body, commits, and touched docs/runbooks. Carry them into the PR-specific todo rather than treating them as a substitute for that todo.

Map every changed behavior or validation claim to the check and job, current SHA, conclusion, actual command, architecture or configuration, case or binary, and observable postcondition. Accept CI as coverage only when the exact current-head job succeeded and evidence shows that it executed the same target behavior. A check name or broad green summary, stale SHA, skipped path or matrix entry, different command/architecture/feature, missing success marker, or no proof that a new test was discovered and executed is not coverage.

For every accepted successful CI item, skip the equivalent local validation. This applies uniformly to apps, QEMU, rootfs, build, clippy, tests, packaging, symbolizers, tool wrappers, and documented workflows. Run local validation only for failed, cancelled, missing, stale, skipped, or suspicious CI coverage, or for a claim CI did not execute exactly; prefer the narrowest command that checks the gap. CI never replaces static code, architecture, ABI, lifecycle, security, documentation, or test-trustworthiness review. Ordinary fixed-head CI supplies only bug-fix green evidence, so separately require unfixed red evidence from test logic, author evidence, dedicated CI, or non-duplicative local baseline validation.

Carry this plan into the per-PR review and final report. The report must distinguish:

- `CI covered`: each accepted successful check and job with exact-head command, architecture/configuration, target behavior, and postcondition;
- `duplicative local validation skipped`: every app, QEMU, build, clippy, test, packaging, or tool command intentionally not rerun because exact current-head CI covered it;
- `CI-missing validated`: exact documented or PR-body workflow, local/manual command run, architecture or target, and observed postcondition;
- `CI-missing not validated`: exact workflow or claim, why it was not run, and whether that limitation blocks approval;
- `bug-fix red/green`: the independent red source and the accepted CI or local green source.

## Dispatch

对每个符合条件的 PR 调用 `review-single-pr`。提示中携带批量审查上下文，但把具体审查决定留给单 PR 技能：

```text
请使用 $review-single-pr 审查 <owner>/<repo> 的 PR #<pr>。

来自 $review-open-prs 的上下文：
- 符合条件的原因：<当前用户从未审查 | 最新提交 <sha/time> 晚于当前用户上次审查 <time>>。
- 草稿状态：<draft|ready>。
- 合并状态：<mergeStateStatus>；维护者能否修改：<maintainerCanModify>。
- 用户要求的范围：<范围摘要>。
- 当前精确提交的 CI 摘要：<head SHA、success/failure/skipped 数量、相关检查名称、陈旧/缺失/可疑说明；确认没有相关运行中任务>。
- 验证计划：<逐项 CI 覆盖台账>；<包括应用和 QEMU 在内、因精确成功 CI 覆盖而跳过的本地命令>；<CI 未覆盖且需验证的 PR 描述或文档流程>；<因 CI 缺失、失败、取消、跳过或可疑而仍须运行的最窄命令>；<错误修复独立 red 证据>。

只审查这个 PR。任何详细审查、任务清单、工作树、本地验证或 PR 写操作前，先按 $review-single-pr 刷新当前 SHA 和相关 CI；若出现 `queued`、`pending`、`waiting` 或 `in_progress` 的相关任务，立即返回暂缓状态且不继续处理。门禁通过后，完整阅读所有适用指令、规范和操作手册，使用可用的任务清单工具创建完整的 PR 专属清单；只有工具不可用或确认失效时才改用可见的 Markdown 清单。按照 $review-single-pr 建立工作树、检查重复或已被取代的实现、处理冲突、执行 CI 未覆盖的针对性验证、检查当前提交 SHA，并提交最终的 APPROVE 或 REQUEST_CHANGES。当前精确提交的成功 CI 已证明执行同一命令、架构或配置和目标行为时，包括 StarryOS/ArceOS 应用与 QEMU 在内都不得本地重复。每条要求继续修改的行内评论或讨论回复都使用 $review-single-pr 的七段中文模板，总审查正文使用四段中文核心结构；提交前检查草稿格式，提交后重新读取实际评论并检查显示格式。提交前以及分配审查人和清理后，都要逐项审计任务清单。
```

明确允许使用执行者或子代理时，给每个执行者恰好一个 PR 和一个工作树。提示中必须要求：

- 使用 `review-single-pr` 执行实际审查流程；
- 在详细审查、任务清单、工作树或本地验证前刷新精确当前提交和相关 CI；存在相关运行中任务时只返回 `CI_DEFERRED`、SHA 和任务状态，不继续处理；
- 完整阅读所有适用指令和资料后，在详细审查前使用可用的任务清单工具创建并维护完整的 PR 专属清单；只有工具不可用或确认失效时才改用可见的 Markdown 清单；
- 只执行只读审查和针对性验证；
- 跳过当前精确提交成功 CI 已证明执行的全部等价本地验证，包括应用、QEMU、构建、clippy、测试、打包和工具流程；只运行 CI 未覆盖的最窄验证；
- 不提交 GitHub 审查；
- 除非明确分配冲突修复，否则不推送贡献者分支；即使分配，也优先只创建本地提交，由主代理最终推送；
- 门禁通过后返回 `APPROVE` 或 `REQUEST_CHANGES`；门禁暂缓时只返回 `CI_DEFERRED`；
- 为每个阻塞问题返回 `path`、`line`、`side=RIGHT` 和中文行内评论正文；
- 每条要求继续修改的评论完整包含“为什么需要改动”“改动收益”“改动前逻辑（基准分支）”“改动后逻辑（当前 PR）”“触发场景与证据”“问题级别”“建议修改方式”七个粗体 Markdown 标题；
- 总审查正文至少包含前四个二级 Markdown 标题，并报告发布前后的格式检查结果；
- 返回前逐字核对固定标题；缺少标题、标题下为空或使用连续段落代替时，丢弃草稿并重写；
- 列出执行的命令和精确失败；
- 汇报每个受影响应用的准备资料来源、架构、运行命令、后置条件和 CI 或本地证据来源，同时汇报已验证和未验证的 CI 缺失流程、精确原因，以及因当前提交 CI 已覆盖而跳过的全部本地验证；
- 指出错误修复缺失的复现测试；
- 返回前审计每个任务清单项，汇报完成证据、具体不适用理由、阻塞结果和未完成项目；
- 清理临时工作树和文件；清理不安全时汇报路径和原因。

提交任何来自执行者的审查前，主代理必须刷新 PR 当前提交，确认每个问题仍适用于当前右侧变更行，并遵循 `review-single-pr` 的提交和发布后格式检查规则。

## Conflict Handling

For each conflicted eligible PR, dispatch through `review-single-pr`; it owns the conflict policy, including repairing conflicts after an otherwise-approvable review when maintainer edits are allowed. If the user explicitly asks for conflict handling, say that in the dispatch prompt. The main agent must keep conflict repair separate from ordinary review, and must not force-push contributor branches.

## Final Summary

End with a concise summary of:

- reviewed PRs, decision, and key reason;
- PRs excluded from review and why;
- CI-deferred PRs with exact head SHA and every unfinished relevant check; confirm that no detailed review, worktree, local validation, or PR mutation was performed;
- batch and per-PR todo reconciliation: completed evidence, concrete not-applicable reasons, blocking items, and unfinished items;
- for every affected app: setup source, architecture, runtime command, observed postcondition, CI or local evidence source, and whether equivalent local execution was skipped;
- for each reviewed PR: exact-head CI-covered evidence, CI-missing PR-body/docs workflows validated locally/manually, CI-missing workflows not validated and why, all CI-equivalent local checks skipped, and bug-fix red/green evidence when applicable;
- validation commands that failed, could not be run, or revealed that a documented workflow does not match the PR's claim;
- any PRs left for the author because of conflicts, missing maintainer edit permission, stale heads, CI gaps, or insufficient local/manual evidence for CI-missing flows;
- temporary worktrees/files that could not be cleaned and why.
