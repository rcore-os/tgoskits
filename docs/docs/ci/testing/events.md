---
sidebar_position: 1
sidebar_label: "事件与调度"
---

# CI 事件与调度

`.github/workflows/ci.yml` 将是否触发工作流、是否分配 runner、是否生成矩阵分成三道判断。触发了一条 Actions 记录，并不意味着它会 checkout 代码或运行测试。

## 1. 仓库事件

`on` 先按事件和路径过滤，`plan_ci.if` 再按 PR 来源决定是否启动规划 job。后者是 job 级条件，而不是占用 runner 后才执行的 Shell 判断。

### 1.1 触发过滤

主 CI 的事件行为由当前 `ci.yml` 声明决定。表中的“完整矩阵”指经过 owner、事件等配置约束后可用的完整检查集，不保证每个命令内部也执行全量检查。

| 事件 | 路径规则 | 后续行为 |
| --- | --- | --- |
| `push` | 排除 Markdown、RST、AsciiDoc 及配置列出的文档目录 | 任意分支均可触发，使用完整矩阵 |
| 同仓 `pull_request` | 只包含声明的源码、配置和测试路径，最后排除 Markdown | 先尝试复用同 SHA push，无法复用时按 PR diff 规划 |
| 跨仓 `pull_request` | 可能产生工作流记录 | `plan_ci` 在 runner 分配前跳过，下游矩阵不执行 |
| `workflow_dispatch` | 不受 push/PR 路径过滤限制 | 允许指定 `since_sha`，矩阵仍为完整选择 |

PR 路径是允许列表，不等价于“所有非文档文件”。例如仅修改 Markdown 不会触发主 CI，但显式手动运行仍可执行。命令级增量范围与矩阵选择的区别见[检查规划](planning.md)。

### 1.2 fork 的执行归属

`plan_ci.if` 比较 `github.event.pull_request.head.repo.full_name` 与 `github.repository`。外部 fork 向主仓提 PR 时，两者不同，主仓不启动规划 runner，也不将测试转到主仓的 GitHub-hosted runner。

贡献者向 fork 分支 push 后，由 fork 仓库自己的 `ci.yml` 执行。普通外部 fork 的 owner 不满足组织 profile 的限制，QCS 检查在该 fork 上下文中回退到 GitHub-hosted，组织专用板卡和 KVM 检查不启用。贡献者需要自行启用 Actions 并准备矩阵依赖的容器；主仓不会自动跨仓触发、配置或接管这些任务。

`reusable-check-matrix.yml` 的 `run.if` 独立拒绝跨仓 PR，防止调用方仅修改矩阵绕过来源检查。该执行器同时保留 push、手动、同仓 PR 和 Starry Apps 所需的定时入口。

### 1.3 信任边界

来源条件描述正常工作流的资源归属，不是 PR 无法篡改的组织权限边界。攻击者仍可能修改工作流本身，管理员需要在 runner group 中限制仓库及可信工作流访问；不能仅凭仓库内条件宣称持久化 runner 已完全隔离。

当前实现没有“批准某个 fork SHA 后在主仓运行硬件测试”的流程，也不把 fork CI 状态自动聚合成主仓必需检查。主仓 skipped 与 fork success 必须分别核实，维护者也不应为了绕过限制把未审查提交直接复制到主仓分支。

## 2. 同一提交去重

`Route duplicate events` 将同仓分支的 push run 作为该 head SHA 的首选验证。它按工作流、分支和 SHA 查询，不会仅凭分支名或最近一条成功记录跳过 PR。

### 2.1 复用条件

PR 事件有时早于对应 push 记录可见，因此去重步骤最多查询十轮，尚未找到记录时在轮次之间短暂等待。找到候选后，再查询其当前状态，避免直接使用列表中的旧状态。

| push 状态或查询结果 | PR 的处理 |
| --- | --- |
| `queued` 或 `in_progress`，且未取消 | 认为 push 已承担验证，设置 `should_run=false` |
| `completed`，存在非 `Plan CI` 且非 skipped/cancelled 的实际 job | 复用这条 run；其结论可以是成功或失败 |
| 已取消，或 completed 但只有规划/跳过记录 | 不复用，PR 自己规划 |
| 查询失败、没有匹配记录、其他不满足条件的状态 | 不推断已有覆盖，保留 PR 检查 |

查询失败时保留检查，是对“是否已有验证”的保守处理，不会绕过前面的跨仓来源门禁。失败的 push 仍可能被复用，因为去重判断的是谁承担验证；失败结论仍应从原 push run 读取。

### 2.2 资源效果

`should_run=false` 后，配置校验、checkout 和矩阵规划步骤不执行，下游 caller 也不会生成测试 job。已有的 `Plan CI` runner 仍会完成清理步骤，因此这是节省重复矩阵，不是承诺同仓 PR 完全零 runner 开销。

PR 不会为了去重取消当前 SHA 的 push，避免把正常验证显示成 cancelled。`test_ci_routing.py` 的 `DuplicateEventRoutingTests` 覆盖 API 查询失败、Plan-only push、被取消的 push 和同 SHA 实际矩阵存在等边界。

## 3. 队列与清理

workflow 层并发、matrix 层并发和机器可用容量是三个不同限制。`concurrency` 管理同一组的 run；`max-parallel` 管理矩阵行；runner 标签和在线状态决定任务何时真正开始。

### 3.1 主分支保留规则

`main` 和 `dev` 的非 PR run 使用按 ref 分开的固定 concurrency group，并声明 `queue: max`，保留各次提交顺序执行。两个分支不共用同一条 workflow 队列，但仍可能竞争相同的自托管机器。

PR 和普通分支的 run 使用带 `github.run_id` 的组名，不进入主分支队列。不能为了减少排队直接统一启用 `cancel-in-progress`，否则会改变主分支每个提交都保留 CI 的规则。

### 3.2 复用规划 runner

`Cancel older queued or running runs` 已在 `plan_ci` 中，并位于 checkout 之前。不再使用独立的 `ci-pr-cleanup.yml` 或单独的 `pull_request_target` 清理 runner。

清理步骤查询 queued、in-progress、waiting 和 requested 的旧记录，按 `run_number` 排除当前及更新的 run。同仓 PR 只取消同一 PR 的旧 PR run；缺失 PR 关联时使用 head repository ID 和 head branch 匹配。普通分支的非 PR 事件按相同分支和事件筛选，`main`、`dev` 的非 PR run 不进入清理。

即使当前 PR 已通过 `should_run=false` 复用 push，清理仍会执行。旧 run 先收到普通 cancel，复查尚未结束时再 force-cancel；查询和取消失败会打印 warning，不把基础设施查询故障隐藏成测试成功。

清理复用已有 runner，代价是要等 `plan_ci` 获得执行机会；它不是独立的即时取消服务。当前设计避免为每次内部 PR 再启动一个清理 job，仍保留旧任务回收和主分支提交完整验证。
