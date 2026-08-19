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

对每个符合条件的 PR 调用 `review-single-pr`。提示中携带批量审查上下文，但把具体审查决定留给单 PR 技能：

```text
请使用 $review-single-pr 审查 <owner>/<repo> 的 PR #<pr>。

来自 $review-open-prs 的上下文：
- 符合条件的原因：<当前用户从未审查 | 最新提交 <sha/time> 晚于当前用户上次审查 <time>>。
- 草稿状态：<draft|ready>。
- 合并状态：<mergeStateStatus>；维护者能否修改：<maintainerCanModify>。
- 用户要求的范围：<范围摘要>。
- 当前提交的 CI 摘要：<success/failure/pending/skipped 数量、相关检查名称、陈旧/缺失/可疑说明>。
- 验证计划：<CI 已覆盖的证据>；<每个受影响应用及其文档化环境准备、架构、精确本地运行命令、就绪检查和后置条件>；<其他 CI 未覆盖且需验证的 PR 描述或文档流程>；<因当前提交 CI 已覆盖而跳过的重复非应用检查>；<因 CI 缺失、失败、可疑或 review-single-pr 强制要求而仍须运行的命令>。

只审查这个 PR。完整阅读所有适用指令、规范和操作手册后，在详细审查或验证前使用可用的任务清单工具创建完整的 PR 专属清单；只有工具不可用或确认失效时才改用可见的 Markdown 清单。按照 $review-single-pr 建立工作树、检查重复或已被取代的实现、处理冲突、执行针对性验证、检查当前提交 SHA，并提交最终的 APPROVE 或 REQUEST_CHANGES。即使 CI 已通过，也要在当前提交本地配置并运行每个受影响的 StarryOS 或 ArceOS 应用；文档、准备、就绪或运行失败都必须以精确理由请求修改。每条要求继续修改的行内评论或讨论回复都使用 $review-single-pr 的七段中文模板，总审查正文使用四段中文核心结构；提交前检查草稿格式，提交后重新读取实际评论并检查显示格式。提交前以及分配审查人和清理后，都要逐项审计任务清单。
```

明确允许使用执行者或子代理时，给每个执行者恰好一个 PR 和一个工作树。提示中必须要求：

- 使用 `review-single-pr` 执行实际审查流程；
- 完整阅读所有适用指令和资料后，在详细审查前使用可用的任务清单工具创建并维护完整的 PR 专属清单；只有工具不可用或确认失效时才改用可见的 Markdown 清单；
- 只执行只读审查和针对性验证；
- 跳过仅重复当前提交已通过 CI 的宽泛非应用本地检查，但始终遵循文档化环境准备，并在当前提交本地运行每个受影响的 StarryOS 或 ArceOS 应用；
- 不提交 GitHub 审查；
- 除非明确分配冲突修复，否则不推送贡献者分支；即使分配，也优先只创建本地提交，由主代理最终推送；
- 返回 `APPROVE` 或 `REQUEST_CHANGES`；
- 为每个阻塞问题返回 `path`、`line`、`side=RIGHT` 和中文行内评论正文；
- 每条要求继续修改的评论完整包含“为什么需要改动”“改动收益”“改动前逻辑（基准分支）”“改动后逻辑（当前 PR）”“触发场景与证据”“问题级别”“建议修改方式”七个粗体 Markdown 标题；
- 总审查正文至少包含前四个二级 Markdown 标题，并报告发布前后的格式检查结果；
- 返回前逐字核对固定标题；缺少标题、标题下为空或使用连续段落代替时，丢弃草稿并重写；
- 列出执行的命令和精确失败；
- 汇报每个受影响应用的准备资料来源、准备命令、就绪结果、架构、运行命令、后置条件或阻塞失败，同时汇报 CI 已覆盖证据、已验证的 CI 缺失流程、未验证流程及原因，以及因重复当前提交 CI 而跳过的非应用本地检查；
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
- batch and per-PR todo reconciliation: completed evidence, concrete not-applicable reasons, blocking items, and unfinished items;
- for every affected app: setup source, preparation result, architecture, current-head local runtime command, observed postcondition, or exact blocking reason;
- for each reviewed PR: CI-covered evidence, CI-missing PR-body/docs workflows validated locally/manually, CI-missing workflows not validated and why, and non-app CI-equivalent local checks skipped because current-head CI already passed;
- validation commands that failed, could not be run, or revealed that a documented workflow does not match the PR's claim;
- any PRs left for the author because of conflicts, missing maintainer edit permission, stale heads, CI gaps, or insufficient local/manual evidence for CI-missing flows;
- temporary worktrees/files that could not be cleaned and why.
