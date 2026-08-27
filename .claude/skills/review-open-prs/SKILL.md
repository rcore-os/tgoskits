---
name: review-open-prs
description: Audit open GitHub pull requests in this tgoskits repository, identify non-self PRs that need the current user's review, classify exact-current-head CI as deferred, skipped, successful, or failed from jobs, steps, and logs, avoid ordinary local validation, require a real app run only for directly changed apps/** content not already covered by equivalent CI, track the reviewable batch with the available todo tool, then dispatch each eligible PR through review-single-pr. Use when the user asks to review all open PRs, review non-self PRs, re-review PRs updated after their last review, or coordinate per-PR review worktrees/subagents.
---

# Review Open PRs

## Goal

Find open PRs that actually need the current user's attention, classify their exact-head CI evidence, then review each eligible PR with `review-single-pr`. This skill is only the multi-PR discovery and dispatch layer; single-PR review standards, CI and app validation, inline comments, approval, request-changes, conflict repair, and final submission rules live in `review-single-pr`.

By default, do not re-review every open PR. Review PRs the current user has never reviewed, or PRs whose latest commit is newer than the current user's last submitted review. Include draft PRs unless the user explicitly says to skip drafts.

Respect the global subagent policy: spawn subagents only when the user explicitly asks for subagents, delegation, or parallel agent work. Even when workers are used, the main agent owns the final GitHub review submission unless the user explicitly assigns that authority elsewhere.

## Todo Orchestration Gate

After the eligibility and CI-readiness pass identifies reviewable PRs, create a user-visible batch todo with one concrete item for each eligible PR plus batch-level final head refresh, review submission verification, reviewer assignment, and cleanup. Do not create a PR-specific review item for a `CI_DEFERRED` or `CI_SKIPPED` PR; keep it only in the corresponding read-only summary. When a todo or plan tool such as `update_plan` is available, calling it is mandatory: invoke it and wait for success before dispatch, keep using it, and do not merely recommend it or replace it with a prose checklist. Treat an empty successful response as success unless the tool reports an error. Diagnose or retry a failed tool call before falling back. Use a visible Markdown checklist only when no todo tool is available or the tool is confirmed unusable, and report that reason. If the tool exposes only pending, in-progress, and completed states, mark an item completed only after recording its completed, not-applicable, or blocking outcome and evidence.

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
5. For each otherwise-eligible PR, map changed surfaces and claims to relevant checks and jobs. If any relevant check or job is `queued`, `pending`, `waiting`, or `in_progress`, mark the PR `CI_DEFERRED`, exclude it from dispatch, and stop processing it. Do not wait, poll, create a PR-specific todo or worktree, run an app, inspect code in detail, resolve discussions, submit comments or reviews, create or update issues, repair conflicts, or change reviewers. Record only the PR number, exact head SHA, and unfinished relevant checks for the final summary. Ignore an unfinished job only with a concrete changed-scope reason showing it is unrelated.
6. Treat PRs already reviewed by the current user at the latest commit as excluded unless the user explicitly asks for a fresh pass of already-reviewed PRs.
7. Keep separate summaries of excluded, `CI_DEFERRED`, and `CI_SKIPPED` PRs. Exclusion reasons include self-authored, already reviewed at latest commit, closed, skipped by user scope, or blocked by a stated constraint.

## Validation Strategy

After a PR passes the nonterminal CI gate and before dispatch, build a CI coverage ledger from the exact current head's checks, jobs, steps, logs, changed files, PR body, commits, and touched docs/runbooks. Carry it into the PR-specific todo rather than treating it as a substitute for that todo.

Map every changed behavior or validation claim to the check and job, current SHA, conclusion, actual command, architecture or configuration, case or binary, relevant log evidence, observable postcondition, and failure propagation. Accept CI as coverage only when the exact current-head job succeeded and its workflow plus logs show that it executed the same target behavior. A check name or broad green summary, stale SHA, skipped path or matrix entry, different command/architecture/feature, missing success marker, swallowed failure, or no proof that a new test was discovered and executed is not coverage.

Do not run ordinary local format, build, clippy, test, QEMU test, metadata, packaging, publish, tool, or documented-workflow validation. If any relevant non-app CI item is failed, dispatch it for log attribution and review; if it is cancelled, missing, stale, skipped, suspicious, or not proven by logs, mark the PR `CI_SKIPPED`, exclude it from dispatch, and report the exact gap. The only exception is missing acceptable execution evidence for an app directly added, modified, or renamed into a runnable `apps/**` directory: carry that exact app and target as a real app-run requirement, provided every other relevant CI item is acceptable. CI never replaces static code, architecture, ABI, lifecycle, security, documentation, or test-trustworthiness review. Ordinary fixed-head CI supplies only bug-fix green evidence, so separately require unfixed red evidence from test logic, author evidence, or dedicated CI.

Carry this ledger into the per-PR review and final report. The report must distinguish:

- `CI_DEFERRED`: exact head and every unfinished relevant check or job;
- `CI_SKIPPED`: exact head, every missing, cancelled, stale, skipped, suspicious, or unproven relevant item, and why no review was dispatched;
- `CI covered`: each accepted successful check and job with exact-head command, architecture/configuration, target behavior, log evidence, postcondition, and failure propagation;
- `app evidence`: each directly changed runnable app, its target and command, and whether exact CI covered it, a real local app run supplied it, or a board-only environment remained unavailable;
- `bug-fix red/green`: the independent red source and the accepted current-head CI green source.

## Dispatch

对每个符合条件的 PR 调用 `review-single-pr`。提示中携带批量审查上下文，但把具体审查决定留给单 PR 技能：

```text
请使用 $review-single-pr 审查 <owner>/<repo> 的 PR #<pr>。

来自 $review-open-prs 的上下文：
- 符合条件的原因：<当前用户从未审查 | 最新提交 <sha/time> 晚于当前用户上次审查 <time>>。
- 草稿状态：<draft|ready>。
- 合并状态：<mergeStateStatus>；维护者能否修改：<maintainerCanModify>。
- 用户要求的范围：<范围摘要>。
- 当前精确提交的 CI 摘要：<head SHA、success/failure/skipped 数量、相关检查名称、job/step/log 证据；确认没有相关运行中任务，也没有会触发 CI_SKIPPED 的非应用证据缺口>。
- CI 与应用计划：<逐项 CI 覆盖台账>；<失败任务及待归因日志>；<直接新增、修改或重命名进入 apps/** 的可运行应用>；<同应用、同目标的 CI 证据或待执行真实 app run>；<错误修复独立 red 证据>。

只审查这个 PR。任何详细审查、任务清单、工作树、应用运行或 PR 写操作前，先按 $review-single-pr 刷新当前 SHA 和相关 CI；相关任务未结束时只返回 `CI_DEFERRED`，非应用相关 CI 缺失、取消、陈旧、跳过、可疑或日志不可证实时只返回 `CI_SKIPPED`，两者都不继续处理或提交 GitHub review。门禁通过后，完整阅读所有适用指令、规范和操作手册，使用可用的任务清单工具创建完整的 PR 专属清单；只有工具不可用或确认失效时才改用可见的 Markdown 清单。按照 $review-single-pr 建立工作树、检查重复或已被取代的实现、分析失败 CI、处理冲突、检查当前提交 SHA，并提交最终的 `APPROVE` 或 `REQUEST_CHANGES`。禁止普通本地验证；只有直接改变 `apps/**` 可运行应用且同应用、同目标未被当前 SHA CI 覆盖时运行真实 app。每条要求继续修改的行内评论或讨论回复都使用 $review-single-pr 的七段中文模板，总审查正文使用四段中文核心结构；提交前检查草稿格式，提交后重新读取实际评论并检查显示格式。提交前以及分配审查人和清理后，都要逐项审计任务清单。
```

明确允许使用执行者或子代理时，给每个执行者恰好一个 PR 和一个工作树。提示中必须要求：

- 使用 `review-single-pr` 执行实际审查流程；
- 在详细审查、任务清单、工作树或应用运行前刷新精确当前提交和相关 CI；存在相关运行中任务时只返回 `CI_DEFERRED`、SHA 和任务状态，不继续处理；存在非应用相关 CI 证据缺口时只返回 `CI_SKIPPED`、SHA、检查项和原因；
- 完整阅读所有适用指令和资料后，在详细审查前使用可用的任务清单工具创建并维护完整的 PR 专属清单；只有工具不可用或确认失效时才改用可见的 Markdown 清单；
- 只执行只读审查、CI 日志分析和 `review-single-pr` 允许的真实 app run；
- 不运行本地格式化、构建、clippy、测试、QEMU 测试、元数据、打包、发布或工具流程；只有直接改变 `apps/**` 可运行应用且缺少同应用、同目标 CI 执行时运行真实 app；
- 不提交 GitHub 审查；
- 除非明确分配冲突修复，否则不推送贡献者分支；即使分配，也优先只创建本地提交，由主代理最终推送；
- 门禁通过后返回 `APPROVE` 或 `REQUEST_CHANGES`；门禁未结束时只返回 `CI_DEFERRED`，证据缺口时只返回 `CI_SKIPPED`；
- 为每个阻塞问题返回 `path`、`line`、`side=RIGHT` 和中文行内评论正文；
- 每条要求继续修改的评论完整包含“为什么需要改动”“改动收益”“改动前逻辑（基准分支）”“改动后逻辑（当前 PR）”“触发场景与证据”“问题级别”“建议修改方式”七个粗体 Markdown 标题；
- 总审查正文至少包含前四个二级 Markdown 标题，并报告发布前后的格式检查结果；
- 返回前逐字核对固定标题；缺少标题、标题下为空或使用连续段落代替时，丢弃草稿并重写；
- 列出检查的 CI 命令、job、step、关键日志和精确失败；
- 汇报每个直接变更应用的准备资料来源、架构或板卡、运行命令、后置条件和 CI 或真实 app run 证据；板端未运行时汇报缺少的板卡和剩余风险；
- 指出错误修复缺失的复现测试；
- 返回前审计每个任务清单项，汇报完成证据、具体不适用理由、阻塞结果和未完成项目；
- 清理临时工作树和文件；清理不安全时汇报路径和原因。

提交任何来自执行者的审查前，主代理必须刷新 PR 当前提交，确认每个问题仍适用于当前右侧变更行，并遵循 `review-single-pr` 的提交和发布后格式检查规则。

## Conflict Handling

For each conflicted eligible PR, dispatch through `review-single-pr`; it owns the conflict policy, including repairing conflicts after an otherwise-approvable review when maintainer edits are allowed. Conflict repair runs only conflict-marker and diff-hygiene checks locally, then relies on the pushed head's CI gate and logs; it must not run ordinary local validation. If the user explicitly asks for conflict handling, say that in the dispatch prompt. The main agent must keep conflict repair separate from ordinary review, and must not force-push contributor branches.

## Final Summary

End with a concise summary of:

- reviewed PRs, decision, and key reason;
- PRs excluded from review and why;
- `CI_DEFERRED` PRs with exact head SHA and every unfinished relevant check; confirm that no detailed review, worktree, app run, or PR mutation was performed;
- `CI_SKIPPED` PRs with exact head SHA, every missing, cancelled, stale, skipped, suspicious, or unproven relevant item, and the reason no review was submitted;
- batch and per-PR todo reconciliation: completed evidence, concrete not-applicable reasons, blocking items, and unfinished items;
- for every directly changed runnable app: setup source, architecture or board, runtime command, observed postcondition, CI or real app-run evidence, and any board-only flow left unrun;
- for each reviewed PR: exact-head CI-covered command/job/step/log evidence, related CI failure attribution, and bug-fix red/green evidence when applicable;
- app commands that failed, could not run, or revealed that a documented workflow does not match the PR's claim;
- any PRs left for the author because of conflicts, missing maintainer edit permission, stale heads, or related CI failure;
- temporary worktrees/files that could not be cleaned and why.
