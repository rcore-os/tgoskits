---
name: review-single-pr
description: 审查本 tgoskits 仓库中一个指定的 GitHub 拉取请求。适用于用户指定 PR 编号或网址，要求审查、复审、先检查当前精确提交的 CI 并复用其验证证据、暂缓仍有相关 CI 运行的 PR、对照 Linux/POSIX/RFC/VirtIO 语义、检查重复实现或相关开放 PR、建立并关闭 PR 专属审查清单、验证测试位置与发现和执行链路、修复安全的合并冲突、执行 CI 未覆盖的针对性验证、提交面向初学者的中文行内评论、批准或请求修改，以及审查后依据 .github/MAINTAINERS.md 推荐并分配审查人。
---

# 审查单个 PR

## 强制要求

将本技能视为强制性审查规范，而不是建议清单。触发后完整阅读本文件，再作出审查结论；除非更高优先级指令冲突，否则执行所有适用要求。

判断代码质量、可维护性或可合入状态前，完整阅读 `docs/guideline/code-quality.md`。PR 新增或扩展用户可见行为、共享或公共接口、软件包、子系统、平台或硬件能力时，完整阅读 `docs/guideline/feature-development.md`；按语义而不是标题判断是否适用。仅在语义适用时读取其他领域规范。任何改动或声明若影响 StarryOS 系统调用或 Linux ABI，包括任务、虚拟文件系统、命名空间、信号、套接字、凭据、内存管理等间接辅助代码，完整阅读 `docs/guideline/starry_syscall.md`。不适用时，在审查清单中记录具体理由。上下文被压缩、从摘要恢复或无法确信记得规范时，重新完整阅读，不能依赖记忆或旧的局部阅读。

没有完整阅读本技能和所有适用规范时，不得提交 `APPROVE`、`REQUEST_CHANGES`、不提交审查的总结或任何面向 PR 的评论。唯一例外是“当前提交 CI 前置门禁”发现相关 CI 仍在运行时，只向用户报告暂缓状态；该状态报告不是审查结论，也不得写入 PR。规则重叠时采用更严格者；跳过要求时记录具体理由和证据。

输出任何审查文本前，先执行“中文审查文本规范”。要求修改的评论先复制“为什么需要改动、改动收益、改动前逻辑（基准分支）、改动后逻辑（当前 PR）、触发场景与证据、问题级别、建议修改方式”七个粗体 Markdown 标题再填写；缺少任一标题、标题下为空或用连续段落代替标题时，禁止输出或提交，必须重写。总审查正文先复制前四个二级 Markdown 标题再填写；缺少任一标题时同样禁止输出或提交。

## 审查清单门禁

在线 PR 在详细判断、完整加载领域规范、创建清单、建立工作树或运行本地命令前，先按“当前提交 CI 前置门禁”读取最少的当前提交、变更路径和检查状态。相关 CI 仍在运行时立即暂缓，不创建 PR 专属清单，也不继续处理该 PR。

CI 门禁通过后，读取足以识别审查范围的 PR 描述、提交记录和语义范围，然后完整读取本技能、仓库指令、必读规范以及规划验证所需的应用文档或操作手册。

完成这些读取后，立即通过可用的任务清单工具创建用户可见、PR 专属的完整清单，并等待调用成功。持续使用同一个工具，最多一个项目处于进行中；发现新范围时先追加清单再调查。工具返回空结果但未报错时视为成功。只有工具不可用或确认失败时才改用可见的 Markdown 清单，并说明原因。

每个清单项写明具体受影响范围和预期证据，覆盖：当前提交信息、既有审查讨论、CI 覆盖台账、工作树、必要时的冲突、每个受影响模块及审查视角、代码质量基线、功能开发规范适用性、领域语义、测试的位置/构建/发现/选择/执行、重复与重叠分析、CI 未覆盖的精确验证命令、阻塞问题与评论、当前提交刷新、审查提交、审查人分配和清理。每个受影响应用及每个新增或迁移测试都建立独立证据项目；已被当前提交成功 CI 精确覆盖的项目以检查名、任务、命令、架构或配置和后置条件关闭为“不需要重复本地验证”。禁止使用“审查代码”“运行测试”之类泛化项目。

提交任何审查结论前逐项审计，只能以“有证据地完成”“给出具体理由的不适用”或“有证据的阻塞结论”关闭。阻塞结论会完成调查项，但必须进入中文审查文本和最终决定。任何必需项仍为 `pending`、不可验证或缺少证据时禁止 `APPROVE`；若缺口由 PR 引入则提交 `REQUEST_CHANGES`，若外部审查系统限制阻止完成则明确不提交审查。提交审查、分配审查人和清理后，再做一次最终清单审计，向用户汇报完成项、不适用项、阻塞项和未完成项。

## 离线基准模式

仅当以精确参数 `offline-benchmark` 调用，且仓库存在 `.agent-review-context/reviewer.md` 时启用。否则执行正常在线流程。

以 `bench-base..HEAD` 为唯一被审变更。完整阅读本技能、`AGENTS.md`、`docs/guideline/code-quality.md`，按需读取 `docs/guideline/feature-development.md` 和领域规范，并读取离线约定与输出格式。应用本技能的审查重点、测试质量、阻塞问题、硬件/ABI、安全与健全性、可维护性和文档要求。

离线环境没有真实 PR：PR 元数据、审查讨论、远端 CI、开放 PR 搜索、工作树、冲突修复、联网语义研究、命令验证、GitHub 提交、审查人分配和远端清理均标为不适用。禁止推断 PR 编号、访问仓库外路径或网络、修改文件、创建提交或分支、运行构建或测试。只使用只读仓库检查和测试框架允许的 Git 历史与差异命令。

只返回 `.agent-review-context/review.schema.json` 要求的 JSON。问题必须由 `bench-base..HEAD` 引入并锚定 `HEAD` 侧变更行；没有问题时返回空 `findings`。禁止提交或起草任何面向 GitHub 的审查文本。仍须创建并审计清单；若无任务清单工具，在内部跟踪，不能破坏只返回 JSON 的约定。

## 目标与工具优先级

只审查指定的一个 PR，先复用当前精确提交的 CI 证据，再在隔离工作树中完成代码分析和 CI 未覆盖的必要本地验证；同时判断它是否重复基准分支已有功能、与其他开放 PR 重叠、冲突或已被取代。没有阻塞问题时提交 `APPROVE`；存在正确性、规范、重复、测试或 CI 覆盖问题时，以中文行内评论提交 `REQUEST_CHANGES`。审查完成后，仅在仍需领域跟进时依据 `.github/MAINTAINERS.md` 分配合适的人类审查人。

本技能是 `review-open-prs` 的单 PR 权威流程。不要完整审查所有开放 PR，但读取足够的相关 PR 上下文来分类重复和重叠。

GitHub 操作优先遵循系统技能：

- `github:github`：仓库定位、PR 元数据、补丁、评论、标签、反应和连接器优先行为；
- `github:gh-address-comments`：未解决讨论、请求修改、行内上下文、锚点和讨论解决状态；
- `github:gh-fix-ci`：失败的 GitHub Actions 检查和日志。

优先使用 GitHub 连接器获取结构化数据，本地 `git` 用于获取、工作树、差异和验证；只有连接器无法满足当前分支发现、GraphQL 讨论、Actions 日志或带锚点提交等需求时才使用 `gh`。

## PR 信息收集

1. 通过 `github:github` 获取仓库身份、当前用户、PR 编号或网址、标题、描述、作者、基准与来源分支、`headRefOid`、草稿状态、合并状态、变更文件、补丁、提交、既有审查或评论和检查结果。
2. PR 作者是当前 GitHub 用户时，提交正式审查前先征询用户。
3. 除非用户明确排除，否则包含草稿 PR。
4. 创建工作树前确保连接器状态和本地检出内容一致。

连接器缺少必要数据时才回退：

```bash
gh auth status
gh repo view --json nameWithOwner,defaultBranchRef,url
gh pr view <pr> --json number,title,body,author,baseRefName,headRefName,headRefOid,headRepositoryOwner,isDraft,mergeStateStatus,maintainerCanModify,reviewDecision,url,commits
gh pr diff <pr> --patch --color=never
gh pr checks <pr> --watch=false
gh api --paginate "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
gh api --paginate "repos/<owner>/<repo>/pulls/<pr>/files?per_page=100"
```

## 当前提交 CI 前置门禁

本节只适用于存在真实在线 PR 的正常模式；离线基准模式保持原约定。先解析当前精确 `headRefOid` 或 `head.sha`，再用最少的变更路径和 PR 声明判断哪些检查与本 PR 相关。任何详细代码审查、领域规范加载、任务清单、工作树、本地验证或 PR 写操作都必须等待本门禁完成。

优先查询 `statusCheckRollup`、检查套件和检查运行；REST 回退也绑定同一 SHA。不能单独用传统的 `GET /repos/<owner>/<repo>/commits/<sha>/status` 判断 Actions 状态，因为它可能显示 `pending` 且 `statuses` 为空，而 Actions 已结束。

```bash
gh pr checks <pr> --repo <owner>/<repo> --watch=false
gh api --paginate "repos/<owner>/<repo>/commits/<head-sha>/check-runs?per_page=100"
gh api --paginate "repos/<owner>/<repo>/actions/runs?head_sha=<head-sha>&per_page=100"
gh api --paginate "repos/<owner>/<repo>/actions/runs/<run-id>/jobs?per_page=100"
```

若任一相关检查或任务为 `queued`、`pending`、`waiting` 或 `in_progress`，立即将该 PR 标记为暂缓并停止。不得等待或轮询到结束，不得完整审查、创建工作树、运行本地验证、创建 PR 专属清单、解决讨论、提交评论或审查、创建或更新议题、修复冲突或更改审查人；只向用户报告 PR、当前 SHA、未结束检查名和状态。无关发布任务或按变更范围明确不适用的任务不触发暂缓，但必须记录不相关理由。

没有相关运行中检查时，先建立 CI 覆盖台账，再规划本地验证。每项台账记录受影响行为或声明、检查与任务名、当前 SHA、结论、实际命令、架构或配置、用例或二进制，以及可观察后置条件。只有 `success` 且证据表明当前精确提交实际执行了同一命令、架构或配置和目标行为时，才接受为覆盖；检查名或宽泛汇总绿灯、旧 SHA、路径或矩阵跳过、架构或 feature 不同、命令不等价、未达到成功标记，或无法证明新增用例被发现和执行，都不算覆盖。摘要不足时检查工作流定义和任务日志。

对台账中已接受的 CI 覆盖，不得再运行等价本地验证，包括应用、QEMU、构建、clippy、测试、打包和工具流程。CI 失败、取消、缺失、陈旧、跳过或覆盖可疑时进入后续分类，并只为归因或未覆盖声明安排最窄的本地验证。CI 证据只替代重复执行，不替代代码、架构、ABI、生命周期、安全性、文档和测试可信度审查。

## 审查讨论与终态 CI 分类

涉及既有请求修改、未解决讨论、行内位置或解决状态时，遵循 `github:gh-address-comments`。扁平评论列表不能代表完整讨论状态。需要时使用带分页的完整 GraphQL 查询：

```bash
gh api graphql --paginate \
  -F owner="$owner" -F repo="$repo" -F number="$pr" \
  -f query='query($owner:String!,$repo:String!,$number:Int!,$endCursor:String){repository(owner:$owner,name:$repo){pullRequest(number:$number){reviewThreads(first:100,after:$endCursor){nodes{id isResolved isOutdated path line diffSide comments(first:100){nodes{author{login} body createdAt}pageInfo{hasNextPage endCursor}}}pageInfo{hasNextPage endCursor}}}}}'
```

若任一讨论的 `comments.pageInfo.hasNextPage=true`，再以该讨论的 `id` 分页取得剩余评论，不能把前 100 条当作完整讨论：

```bash
gh api graphql --paginate \
  -F threadId="$thread_id" \
  -f query='query($threadId:ID!,$endCursor:String){node(id:$threadId){... on PullRequestReviewThread{comments(first:100,after:$endCursor){nodes{author{login} body createdAt}pageInfo{hasNextPage endCursor}}}}}'
```

检查所有未解决讨论。具体问题已在当前提交修复时解决讨论；修复不完整、测试未接入运行器或评论仍有效时保持未解决。操作后重新查询并确认 `isResolved=true`：

```bash
thread_id='<thread-id>'
gh api graphql \
  -f query='mutation($threadId:ID!){resolveReviewThread(input:{threadId:$threadId}){thread{id isResolved}}}' \
  -f threadId="$thread_id"
```

区分预期的矩阵或路径过滤 `skipped` 与整个相关工作流未运行。本仓库互斥的 `run_host`/`run_container`、分支限制发布任务或路径过滤任务可以为 `skipped`，其成功的同级任务足以说明工作流运行。以 `success=N, skipped=M, failure=0` 汇报，并命名关键检查。只有变更范围应由该检查覆盖、路径过滤跳过必需覆盖或所有相关检查均被跳过时，才把 `skipped` 视为可疑。

提交审查前检查每个失败、取消、缺失或可疑检查的日志，并分类为“与 PR 相关”“与 PR 无关”或“无法确定”：

- 与 PR 相关：失败任务覆盖本 PR 的文件、软件包、用例、命令、平台或行为；在 PR 当前提交可复现而基准分支不失败；或新增/修改的测试、配置、工作流导致失败、挂起、跳过或超时。提交 `REQUEST_CHANGES`，说明检查项、失败模式、归因和修复方向。
- 与 PR 无关：提供具体证据，例如变更范围外、基准分支同样失败、已知偶发失败、基础设施问题或已有议题。用工作流或任务、特征错误、运行器或平台、用例或命令等多个关键词搜索并检查候选议题；更新合适的现有议题，或在确无匹配时创建唯一议题，并在审查正文链接它。
- 无法确定：合理检查后因果仍不清楚时，禁止仅凭 CI 批准；根据证据请求修改，或在用户只要求调查时明确不提交审查的阻塞原因。

```bash
gh pr checks <pr> --repo <owner>/<repo> --watch=false
gh run view <run-id> --repo <owner>/<repo> --log-failed
gh issue list --repo <owner>/<repo> --state open --search '<workflow or job name>'
gh issue list --repo <owner>/<repo> --state open --search '<distinctive error excerpt>'
gh issue list --repo <owner>/<repo> --state open --search '<runner platform, case, or command>'
gh issue view <issue-number> --repo <owner>/<repo> --comments
gh issue comment <issue-number> --repo <owner>/<repo> --body-file issue-update.md
gh issue edit <issue-number> --repo <owner>/<repo> --title '<updated neutral title>' --body-file issue.md
gh issue create --repo <owner>/<repo> --title '<neutral CI issue title>' --body-file issue.md
```

日志下载为空时不能推断通过或无关；用 `gh pr checks` 和 `gh run view <run-id> --json headSha,jobs` 确认当前提交、失败任务、结论和步骤。

## 工作树

获取 PR 和基准分支，然后在分离状态工作树中审查：

```bash
repo_root="$(git rev-parse --show-toplevel)"
repo_parent="$(dirname "$repo_root")"
review_wt="$repo_parent/$(basename "$repo_root")-review-pr<pr>"
git fetch origin '+refs/pull/<pr>/head:refs/remotes/origin/pr/<pr>' '+refs/heads/*:refs/remotes/origin/*'
git worktree add --detach "$review_wt" origin/pr/<pr>
```

已有工作树仅在无改动且位于当前 PR 提交时复用：

```bash
git -C "$review_wt" status --short
git -C "$review_wt" rev-parse HEAD
git rev-parse refs/remotes/origin/pr/<pr>
```

陈旧且无改动时无损更新；有本地改动时新建工作树或询问用户。禁止修改或回滚用户主工作树。并行审查不同 PR 时使用不同工作树；同一检出目录内不得并发运行多个 StarryOS QEMU 用例。

## 合并冲突

仅在用户明确要求，或审查没有其他阻塞问题、本应 `APPROVE` 且当前 `mergeStateStatus=DIRTY`、`maintainerCanModify=true` 时修复。修复并推送、重新验证新提交前不得批准。

只有 `reviewDecision=APPROVED` 才代表当前汇总批准。历史 `APPROVED` 审查只能作为上下文；汇总批准为空、为 `CHANGES_REQUESTED`，或仍有未解决讨论时，冲突修复只能做不提交的本地演练，除非用户明确要求推送修复。

先刷新 PR 元数据、审查和远端当前提交。`mergeStateStatus=UNKNOWN` 时等待并重查。`DIRTY` 且 `maintainerCanModify=false` 时不得修复：用户明确要求处理冲突时提交 `REQUEST_CHANGES`，说明作者需合并或变基到最新基准分支，并建议启用 Allow edits by maintainers；否则在正文或总结中记录限制。`DIRTY` 且可修改时使用独立冲突工作树，并确认派生仓库分支仍等于 `headRefOid`。

```bash
gh pr view <pr> --json number,baseRefName,headRefName,headRepositoryOwner,headRefOid,mergeStateStatus,maintainerCanModify,reviewDecision,reviews
gh api --paginate "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
git fetch origin '+refs/pull/<pr>/head:refs/remotes/origin/pr/<pr>' '+refs/heads/<base>:refs/remotes/origin/<base>'
git ls-remote "https://github.com/<head-owner>/<repo>.git" "refs/heads/<headRefName>"
git worktree add --detach "$conflict_wt" origin/pr/<pr>
git -C "$conflict_wt" merge --no-ff --no-commit "origin/<base>"
git -C "$conflict_wt" diff --name-only --diff-filter=U
```

在分离状态的冲突工作树中，暂存区第 2 阶段（`ours`）是 PR，第 3 阶段（`theirs`）是基准分支；不清楚时使用 `git show :1:<path>`、`:2:`、`:3:`。按 PR 意图和当前基准语义解决，禁止简单保留两边或复活基准分支已替换的 API。PR 837 是参照：保留 `/proc/kallsyms` 功能，但适配 `SeqObject` 与 `SpecialFsFile::new_regular_with_perm`，并同时保留 `ktracepoint`/`ksym`、`.tracepoint`/`.kallsyms` 等独立改动，而不是恢复旧 `SeqFile`。

提交修复前运行格式化、冲突标记扫描、差异卫生检查和针对性验证。解决 `Cargo.lock` 冲突时先处理其他文件，再由 Cargo 重新生成，禁止手工拼接。

```bash
cargo fmt
rg -n '<<<<<<<|=======|>>>>>>>' <conflicted-files>
git -C "$conflict_wt" diff --check
<targeted cargo xtask/cargo test/cargo clippy commands>
git -C "$conflict_wt" add <resolved-files>
git -C "$conflict_wt" commit
```

推送前确认合并提交第一父节点仍是当前 `headRefOid`，并再次执行 `git ls-remote`。远端变化时停止并重新审查。只能普通推送，禁止强制推送：

```bash
git push https://github.com/<head-owner>/<repo>.git HEAD:<headRefName>
```

推送后刷新 PR，并对新 `headRefOid` 重新执行“当前提交 CI 前置门禁”。相关 CI 仍在运行时立即暂缓，不更新审查工作树、不执行本地验证也不批准；CI 成功精确覆盖的验证继续跳过本地重复，只补充新提交上仍未覆盖的最窄验证。冲突消失后，`BLOCKED`/`UNSTABLE` 仍可能由 CI 或审查状态导致，不能仅据此判定冲突修复失败。只做冲突演练时不得推送或提交审查；记录 PR、批准状态、冲突文件、语义解法、验证结果和未修改 GitHub 的事实，然后清理工作树。

## 审查重点

按 PR 意图、当前基准分支、项目既有模式和适用外部语义理解完整实现逻辑：

- 系统调用、进程/会话/信号、文件系统错误码、套接字、IPv4/IPv6 对照 POSIX/Linux；
- 网络行为对照 RFC/Linux，包括 IPv6 NDP、IPv4 映射 IPv6、双栈、路由或监听冲突和错误码；
- 驱动改动检查 VirtIO、PCI、DMA、MMIO、中断和所有权；
- Axvisor 配置检查 `entry_point`、`kernel_load_addr`、`memory_regions`、`map_type` 和客户机镜像布局；
- Starry 测试改动应用 `starry-test-suit`；可移植驱动或操作系统适配层改动应用 `cross-kernel-driver`。

影响 StarryOS 系统调用或 Linux ABI 时，按 `docs/guideline/starry_syscall.md` 的证据层级追踪间接辅助代码到每个受影响的系统调用入口；行为随版本变化时记录对照的 Linux 版本或提交。

### 新功能设计门禁

新增或扩展功能时，按 `docs/guideline/feature-development.md` 分类为局部、共享或高风险，并在清单记录分类和证据位置。按以下顺序审查：必要性、重复性、语义与既有方案、替代方案、整体架构或 API、实现、验证与交付。

核对具体问题、目标用户或调用方、真实场景、成功标准、不包含项、仓库内部研究、适用的权威外部研究、现实替代方案和不实现成本。高风险功能必须有可独立审查的设计材料，覆盖适用的所有权、依赖、兼容性、迁移、回滚、可观测性、性能和安全。先提交重大设计阻塞问题，再处理低层细节。测试通过不能替代“为什么项目需要它、为什么优于复用或扩展、为什么现在值得承担复杂度”的解释。

### 审查视角与问题纪律

优先找全变更范围内的真实缺陷，不为简短而漏报，也不臆造问题。对可疑缺陷构造具体输入、并发交错、设备状态、客户机配置或测试路径；若场景不可能则说明原因。

除非变更显然不涉及，否则应用五类审查视角：

- 可维护性：流程、提交卫生、范围、软件包或模块边界、命名、可见性、注释和可理解性；
- 正确性：正常路径、错误路径、并发、热路径、边界偏差、可达的 `unwrap`/`expect`/`panic`、溢出、错误判断条件、保护条件、唤醒和资源泄漏；
- 安全与健全性：`unsafe` 契约、指针来源、别名、用户内存、信任边界、权限、检查与使用时序竞争、释放后使用；
- 硬件与 ABI：汇编、目标 JSON、陷阱与上下文、SMP 启动、MMIO/DMA/中断、缓存一致性、VirtIO/PCI、设备树或配置、调用约定与对齐；
- 文档与用户可见兼容性：文档、操作手册、应用流程、测试套件指南、兼容性说明和用户可见行为。

同一根因不要在每个视角重复报告；多个症状可以分别锚定，但只完整解释一次共享修复。提交前复核所有承重前提、引用代码和权威外部来源；明确不确定性，撤回前提错误的问题。

### 测试与行为门禁

错误修复必须有确定性回归或复现：未修复实现必然失败，修复后同一测试通过；除非有具体证据说明环境不可能做到，否则缺少失败/通过证明即阻塞。普通的当前提交成功 CI 只证明修复态 green；未修复态 red 仍须由测试逻辑证明、作者提供的 red/green 记录、明确执行未修复变体的 CI，或非重复的本地基准验证提供。原始系统调用修复优先直接覆盖 `syscall(SYS_...)`，避免 libc 封装掩盖返回值或错误码。

新增行为、语义变更、错误修复或新暴露路径必须在正确项目层级有测试。不能只看到测试文件：验证运行器能够发现、构建或安装、选择、执行，并且回归时会失败。错放、孤立、仅手工执行、可选执行或被 CI 静默跳过的测试按缺失处理。

StarryOS 应用支持分层：

- 面向操作人员的冒烟、演示、根文件系统、板卡或 QEMU 脚本、长运行或可选流程放 `apps/starry/<app-or-scenario>/`；
- 内核 ABI、系统调用、文件系统、进程、网络或错误修复语义覆盖放 `test-suit/starryos/<case>` 或既有分组封装；
- 系统调用变化必须有直接测试套件回归；应用冒烟不足以证明系统调用；
- 应用暴露的内核错误尽量提取为无需完整应用的测试套件回归，应用场景保留为集成证据。

每个新增、变更或 PR 明确声明支持的 StarryOS/ArceOS 应用，都建立独立证据项，列明文档化环境准备、架构、运行命令和可观察后置条件。当前精确提交的成功 CI 若证明实际执行了同一准备、命令、架构或配置和目标行为，则关闭为 CI 已覆盖，不得本地重复；否则按文档运行最窄的缺失流程。通用改动只补测 CI 未覆盖的最高风险声明架构；架构特定改动只补测每个新增或变更但未被 CI 覆盖的架构。文档本身无法让用户准备环境、需要未记录的临时绕过或声明范围与可复现证据不一致时，仍提交 `REQUEST_CHANGES`。

禁止测试外形的伪修复、硬编码特例、伪状态、空操作兼容层或未实现真实语义的逻辑。成功路径测试遇到 `ENOMEM`/`EAGAIN` 等意外失败时不得静默返回；合法跳过必须打印明确标记并解释原因。禁止删减用例、架构，放宽 `success_regex`/`fail_regex`，把失败变成跳过或超时，修改路径过滤跳过相关覆盖，或把 CI 覆盖移到仅手工执行，除非有等价或更强且已验证的替代。

Starry QEMU 失败必须传播到 `cargo xtask starry test qemu ...`：封装脚本在命令后立即保存 `$?`，失败时打印 `STARRY_GROUPED_TEST_FAILED` 或配置标记，不得再打印全部通过标记，并让外层命令失败。`success_regex`/`fail_regex` 必须可靠分类。当前 `qemu/system` 分组 C 子用例的 `CMakeLists.txt` 与 `src/` 必须直接位于 `system/<subcase>/`；`system/<subcase>/c/` 默认阻塞，除非同时更新根 `CMakeLists.txt`、运行器发现逻辑、指南和规则测试并验证。

## 可发布 Cargo 补丁策略

PR 触及 `Cargo.toml`、`Cargo.lock`、已提交的 `.cargo/config`/`.cargo/config.toml`、依赖元数据、重复版本、第三方 API 或跨依赖类型边界时，检查所有 `[patch]` 和变更的依赖来源。按来源是否能由本仓库或 crates.io 重现、工作区是否可发布来判断；存在 `[patch.crates-io]` 本身不阻塞。

允许但必须通过解析与发布检查的来源：

- 相对于声明清单或配置解析并规范化后仍位于当前仓库内的 `path`；
- crates.io 已发布的精确版本，包括普通依赖中的 `version = "=1.2.3"`，以及用该版本替代其他来源的注册表补丁。

以下情况阻塞：任意 `git`、绝对 `path`、逃逸仓库的相对路径、非 crates.io 注册表；元数据未解析到预期软件包、版本或来源；请求版本未发布；依赖统一破坏 API 或类型语义；完整工作区发布演练失败。

发布软件包可使用 `{ path = "...", version = "..." }`，打包时 Cargo 使用 crates.io 版本要求；只有 `path` 的普通依赖对需要发布的软件包是阻塞项。根 `[patch]` 中的仓库相对路径自身不要求版本回退，但发布软件包的普通依赖声明仍然要求。

补丁若只为掩盖依赖方与工作区各自拥有的类型不一致，优先使用正常 crates.io 解析和显式边界：使用依赖公开类型；在边界添加软件包私有适配器；使用 `.map_err(...)`、`TryFrom`、封装新类型或扩展 trait；未知错误码提供明确回退。根 `[patch.crates-io] ax-errno = { path = "components/axerrno" }` 的来源形态允许，并可在元数据与完整发布演练证明时统一发布依赖图；若目的只是让 `kbpf-basic` 错误与另一份本地错误类型隐式互换，则保留 `kbpf_basic::BpfError`/`BpfResult` 到 `LinuxError`/`AxError`/`AxResult` 的显式转换。

## 重复与重叠分析

每个 PR 必做。先建立意图指纹：标题、描述、议题、提交、变更的软件包/模块/测试/配置/CI/生成资产、公共 API、系统调用、错误码、协议、设备、运行器、功能，以及功能、修复、覆盖、重构、配置、CI、依赖元数据等语义声明。

先查当前基准分支是否已有等价或更新实现，再用多个意图指纹关键词搜索开放 PR；不能只搜标题。读取候选的意图、文件和差异后分类：

- 重复：同一问题或同一 API、测试、配置，无实质差异；
- 部分重叠：同一受影响范围，但互补、可排序或可拆分；
- 冲突风险：修改同一契约、运行器、生成资产或 ABI，存在合并或语义冲突；
- 已被取代：基准分支或其他 PR 更完整、更符合项目方向；
- 检查后无关：关键词命中但审阅后无关。

```bash
git grep -n -E '<relevant symbols|paths|commands>' origin/<base> -- <likely paths>
git log --oneline --decorate -- <likely paths>
gh pr list --state open --limit 200 --search '<symbol OR path OR issue keyword>'
gh pr view <related-pr> --json number,title,body,author,baseRefName,headRefName,isDraft,updatedAt,files,commits
gh pr diff <related-pr> --patch --color=never
git diff --name-only origin/<base>...origin/pr/<related-pr>
```

依赖另一 PR 先落地时，在描述或审查中明确依赖前不得批准。重复或已被取代时请求修改，或中性说明应优先采用的基准实现或 PR。使用 `git diff origin/<base>...origin/pr/<pr>` 查看 PR 补丁；只有检查陈旧分支影响时才用 `..`。用户要求关闭时，先执行 `gh pr comment <pr> --body-file comment.md`，再执行 `gh pr close <pr>`。

## 验证

验证必须匹配变更范围。选择任何本地命令前先查 CI 覆盖台账；已被当前精确提交成功 CI 精确覆盖的等价命令标记为跳过，并记录检查、任务和覆盖细节，不得重复运行。只为 CI 未覆盖、失败、取消、缺失、陈旧、跳过或可疑的范围安排最窄本地命令，并优先使用项目 `cargo xtask`：

```bash
cargo fmt --check
cargo xtask clippy --package <crate>
cargo clippy --manifest-path <path>/Cargo.toml --all-features -- -D warnings
cargo xtask starry test qemu --arch <arch> -c <case>
cargo xtask axvisor build ... --vmconfigs <config>
```

特殊配置无法由 `xtask` 覆盖时，先检查 `xtask` 帮助和源码，再用参数完全匹配的原生 Cargo 命令。记录精确命令与失败。

依赖元数据变更必须扫描补丁，并取得元数据、依赖树和完整工作区发布演练证据。当前提交成功 CI 已精确执行对应命令时复用该证据；否则在本地执行缺失命令：

```bash
rg --hidden -n '^\s*\[patch(?:\.|\])' -g 'Cargo.toml' -g '**/.cargo/config' -g '**/.cargo/config.toml' .
cargo metadata --locked --format-version=1 | jq -r '.packages[] | [.name,.version,.source,.manifest_path] | @tsv' | rg '<affected-crate>'
cargo tree -p <affected-package> | rg '<affected-crate>|<boundary-crate>'
cargo publish --workspace --dry-run --no-verify
```

相对路径按声明文件解析并确认规范化后仍在仓库根目录下；精确 crates.io 替代版本必须在元数据中显示 crates.io 注册表来源和精确版本。新增、变更或依赖补丁，或修改可发布工作区软件包的来源时，完整工作区打包或解析演练证据是硬门槛；涉及工作区发布顺序或从路径依赖改写到注册表依赖时，单软件包演练不能替代。`--no-verify` 会跳过软件包验证构建，因此该命令或其 CI 结果不能代替未被覆盖的针对性构建、静态检查或运行验证。

每个受影响应用严格按 PR 描述或变更文档建立并核验执行证据：

1. 分别列出环境准备、架构、运行命令、可观察后置条件，以及对应的当前提交 CI 检查和任务。
2. 文档覆盖软件包、工具链、根文件系统、权限、硬件、凭据、网络服务、环境变量、资产、参数和就绪检查；只能引用完整覆盖该应用的规范章节。
3. 不使用本地知识补充未记录命令，不依赖未说明的机器状态。
4. 当前提交成功 CI 已精确执行相同准备、命令、架构或配置并达到同一后置条件时，记录证据并跳过本地运行；否则本地运行真实的 `cargo xtask starry app qemu ...`、`cargo xtask starry test qemu ...`、`cargo xtask arceos test qemu ...` 或文档封装命令。
5. 无论证据来自 CI 还是本地，都验证客户机标记、应用输出、日志、符号化位置、软件包产物等真实结果；退出码为 0 但未执行行为不算通过。
6. 本地补测且 `tmp/axbuild/rootfs` 为空时，仍尝试文档中的根文件系统或测试命令，让 `xtask` 自动下载；失败则记录并提交 `REQUEST_CHANGES`。
7. 同一工作树内一次只运行一个 Starry QEMU 用例。

同样的 CI 覆盖与补测规则适用于 ArceOS 应用、`apps/**` 演示，以及准备、启动、检查、符号化、解析日志、打包或操作 StarryOS/ArceOS 应用的 QEMU 封装、根文件系统或应用准备工具、符号化工具、日志解析器和打包辅助工具。CI 未执行精确流程时，本地运行真实流程，不能只验证语法、文档、`--help`、解析或构建。若缺失流程确因硬件、凭据、服务或宿主能力不可用，记录限制并要求受控回退或其他可复现证据。

分组 QEMU 新增或迁移测试必须核对 `test_commands` 的发现与安装、`/usr/bin/<test>`、`status=127`、子用例选择、功能门控和正则表达式。至少取得以下一种证据：当前提交本地运行、当前提交 CI 明确显示该用例或二进制执行，或确定性构建与发现检查。汇总 CI 通过不足以证明测试未被跳过。检查 shell 封装失败分支和分组错误修复断言，不能只运行成功路径。

对每个新增或迁移测试，不限于分组 QEMU，都写明实际执行它的运行器命令，并取得以下至少一种当前提交证据：本地执行；CI 日志明确显示具体用例、子用例或二进制执行；或确定性构建与发现检查证明运行器一定到达该测试。宽泛的汇总 CI 通过不能证明测试未被路径布局、过滤器、安装规则、子用例选择或功能门控跳过。

应用支持同时包含系统调用或内核错误修复时，应用流程与对应 `cargo xtask starry test qemu` 分别取得当前提交执行证据；每项可由精确 CI 独立覆盖，只本地补充缺口。没有测试变更时，若 PR 描述或提交声称 QEMU、宿主单元测试、`xtask`、静态检查、脚本、模拟器等非实体板卡验证，先核对 CI 是否精确执行；已覆盖则不复跑，未覆盖时再本地执行并核对命令、目标、输出和通过条件。不可复现、静默跳过、目标更窄或失败时请求修改。既无测试又无可复现的非实体板卡验证时禁止批准。仅实体板卡证据不能单独满足此门槛，除非用户明确限定审查范围。

远端 CI 是必需证据但不是唯一证据；没有检查不等于通过。当前精确提交的成功 CI 可以替代其精确覆盖的本地执行，但不能替代静态分析、语义审查、覆盖真实性检查，也不能自动提供错误修复的未修复态 red 证据。

## 阻塞问题

相关 CI 仍为非终态时不进入阻塞判断，而是按前置门禁暂缓并停止处理。CI 已无相关运行中任务后，除非有明确证据表明不阻塞，否则以下情况阻塞：

- 与 POSIX/Linux/RFC/VirtIO 语义不符；
- 新功能缺少问题、用户或调用方、成功标准、不包含项、内部重复搜索、适用权威研究或现实替代方案；
- 高风险功能缺少可独立审查设计或合格领域审查人；
- 跨层捷径、硬编码特殊路径、重复真相源、伪成功、静默回退、无当前使用者的投机 API、配置或扩展；
- 针对性测试、格式化、静态检查或与 PR 相关的 CI 失败；
- 当前提交的 StarryOS/ArceOS 应用或 QEMU 用例在 CI 或必要本地补测中按文档失败，或失败未传播到 `xtask`；
- 应用或 QEMU 声明只验证发现、TOML 解析、旧提交或他人结果；
- 任何受影响应用缺少当前精确提交上覆盖同一准备、命令、架构或配置和后置条件的成功 CI 或本地执行证据，或文档缺环境、命令、参数、就绪条件；
- CI 未覆盖的必要应用流程需要不可用的硬件、凭据、权限、服务、宿主能力或未记录的临时绕过，且没有其他可复现证据；
- 新行为、语义或错误修复缺测试，或测试错位、未发现、未构建或安装、未选择、未直接覆盖 ABI；
- 覆盖因布局、路径过滤、功能门控、子用例、安装规则或仅手工执行的位置被跳过；
- 无测试变更且无可复现的非实体板卡验证，或声明的验证不可复现或不匹配；
- `success_regex`/`fail_regex` 不能可靠分类；
- 错误修复缺少必然失败与通过的回归或复现，且未证明不可能；
- Cargo 补丁使用 `git`、绝对路径、仓库外路径、非 crates.io 注册表，普通可发布依赖只有路径，请求的 crates.io 版本不存在，解析到非预期软件包、版本或来源，或完整工作区发布演练失败；
- 合并冲突未解决，修复复活过时的基准 API，或推送后的新提交未重新通过 CI 前置门禁和必要的未覆盖验证；
- 应用流程与测试套件的语义覆盖层级错误；
- 仅测试的伪修复未实现真实行为；
- 缓冲区、DMA 内存、队列令牌、中断所有权泄漏、过早释放或跨错抽象层；
- CI 以超时等终态失败、跳过新覆盖，或削弱既有用例、架构、正则、路径过滤或正常回归；
- 重复基准分支、削弱已有实现、与开放 PR 冲突或已被取代；
- 无法解释与候选相关 PR 的差异；
- 必需清单项仍为 `pending`、不可验证或缺少证据或具体不适用理由。

## 中文审查文本规范

所有 GitHub 审查文本，包括总审查正文、行内评论和讨论回复，均使用中文、中性且项目导向的表达。命令、路径、代码符号、接口字段、产品名和标准正式名称可以保留原文；在叙述中用中文说明其作用，不连续堆叠英文术语。面向第一次接触相关模块的读者，先说明对象在调用链中的角色，再说明状态变化、触发条件和可观察结果。禁止使用“请优化”“测试通过”“这里不正确”等缺少原因、逻辑和证据的结论。

### 要求修改的评论模板

凡是要求继续修改代码、测试、文档或配置的行内评论和讨论回复，先复制以下七项粗体 Markdown 标题骨架，再逐项填写具体内容。输出时保留 `**标题**` 语法，不得删除标题、留空、合并成含糊短句、改成连续段落或依赖总审查正文代替：

- **为什么需要改动**：说明当前问题、不修改会产生的实际后果和受影响对象。
- **改动收益**：说明修复后恢复或新增的可观察保证，以及对正确性、可维护性、安全性、兼容性或测试可信度的收益。
- **改动前逻辑（基准分支）**：只说明基准分支在 PR 之前的入口、关键状态变化、所有权或错误传播和最终结果。若属于新增路径或缺失测试，写明基准分支此前没有该路径或覆盖。
- **改动后逻辑（当前 PR）**：只说明当前 PR 已经引入的入口、关键状态变化和结果，并指出问题出现在哪一步。新增测试场景应写“PR 添加了文件，但当前布局使运行器无法发现”；绝不把期望修复后的未来逻辑写在此处，未来逻辑只放“建议修改方式”。
- **触发场景与证据**：给出具体输入、状态、并发交错、设备状态、调用链、日志、测试或规范依据；引用当前提交的路径、行号和符号。
- **问题级别**：说明是否阻塞以及影响范围。阻塞问题明确写出会导致的错误结果、崩溃、死锁、资源泄漏、ABI 不兼容、测试失效或其他可观察后果。
- **建议修改方式**：描述应恢复的语义、顺序、所有权、错误传播或测试契约，以及修改完成后的验收条件。只约束根因和必要边界，不替作者扩展无关重构。

同一根因只在最贴切的评论中完整解释一次。若其他变更行只是同一根因的症状，引用该评论并说明本行的局部影响，不再发布重复的修改要求；若该评论本身仍要求修改，则仍须包含七个标题。纯信息说明和批准结论不强制使用七段模板，但仍须使用中文并给出必要依据。

### 总审查正文

总审查正文先复制 `## 为什么需要改动`、`## 改动收益`、`## 改动前逻辑（基准分支）`、`## 改动后逻辑（当前 PR）` 四个二级 Markdown 标题，再逐项填写，形成完整的整体叙事，并通过路径或评论引用汇总行内问题，避免机械复制。正文还覆盖适用的以下内容：

- PR 改动、功能开发规范适用性、风险分类和设计材料位置；
- 新功能的问题、用户、成功标准、不包含项、研究、替代方案和取舍；
- 实现逻辑、项目语义、验证命令与结果；
- 测试要求、位置、构建、发现、选择和执行证据；
- 每个应用的当前提交、准备文档来源、架构、运行命令、可观察后置条件、证据来源，以及因精确 CI 覆盖而跳过或实际执行的本地验证；
- 审查清单审计、无测试时复核的声明、CI 状态、无关失败证据和跟踪议题；
- 重复与重叠分类、冲突处理、与 PR 相关的 CI 失败和修复方向；
- 错误修复的失败/通过证据、已解决与未解决讨论、未实现或后续项、环境限制。

不能只写“测试通过”。没有阻塞问题的批准正文也说明审查范围、改动前后逻辑、主要收益、验证证据和剩余风险。

### 回复已修复问题

仅确认问题已修复的回复可以缩短为“状态”“逻辑变化”“验证证据”三部分，写明当前提交、原问题如何被修复、修改前后的关键差异和同一回归测试的结果。作者只有解释而没有修改代码或测试时，不把解释当作修复证据。仍要求继续修改或只修复一部分时，使用完整七段模板并保持讨论未解决。

### 发布前后格式检查

发布前逐条检查未发布草稿：

- 中文叙述是否完整，英文是否仅为允许保留的精确标识；
- 必填标题是否齐全且顺序正确，各节是否非空并面向初学者解释上下文；
- 是否残留 `TODO`、`TBD`、`<placeholder>` 或模板说明文字；
- Markdown 标题、列表、空行、代码围栏、反引号和链接是否闭合且层级正确；
- 行内评论的 `path`、`line`、`side=RIGHT` 是否存在并绑定当前 `headRefOid`；
- 总审查正文、行内评论和回复之间是否重复、矛盾或遗漏阻塞问题。

任一检查不满足时丢弃草稿并重写，不得以“语义已经包含”为由省略固定标题。

提交后重新读取实际发布的总审查正文、行内评论和回复，检查 GitHub 上的显示、标题、列表、代码块、链接、内容完整性和讨论状态。接口调用成功不等于格式正确。发现格式错误时优先编辑原评论；无法编辑时发布一条完整修正版，并明确原评论已被替代。格式修正不得改变问题语义或掩盖当前提交已经变化的事实。

## 提交审查

提交前用同一个任务清单工具逐项审计。任何必需项仍为 `pending`、不可验证或无证据时不得 `APPROVE`。PR 导致的测试证据缺失、环境准备失败或应用运行失败进入 `REQUEST_CHANGES`；外部系统阻止提交时明确不提交审查。

通过连接器确认 PR 的 `headRefOid` 未变化；回退命令：

```bash
gh pr view <pr> --json number,headRefOid,reviewDecision
```

当前提交变化时先对新 SHA 重新执行 CI 前置门禁；相关 CI 仍在运行时立即暂缓。门禁通过后再重新获取、更新工作树、在新的右侧变更行复核每个问题，并只运行新提交上仍未被 CI 覆盖且支撑结论所需的验证。

先按“中文审查文本规范”完成草稿和发布前格式检查。优先用连接器一次提交审查事件和带锚点评论；连接器无法保持锚点时使用 REST：

```bash
gh api --method POST repos/<owner>/<repo>/pulls/<pr>/reviews --input review.json
```

请求载荷使用当前 `headRefOid`、`side=RIGHT`；有任何阻塞问题时使用 `REQUEST_CHANGES`，无阻塞问题时才使用 `APPROVE`：

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

禁止提交针对旧提交的问题。提交后重新查询审查和评论，执行发布后格式检查；若期间出现新提交，仅在问题对新提交仍成立时提交后续审查。

```bash
gh pr view <pr> --json number,reviewDecision,latestReviews
```

## 推荐审查人分配

仅在提交审查后仍需领域跟进时请求审查人。读取 `.github/MAINTAINERS.md`；它是本地事实来源和自动人类审查人的严格允许列表。只有 `R:` 可自动请求；`M:` 只是所有权元数据，除非同一账号也在 `R:`。不得推断或请求允许列表外的人类审查人。

用 PR 标题、描述、变更路径、API、测试、验证、问题、软件包、配置、功能和差异中可见的标识符匹配 `F:`/`K:`。多个部分命中时请求所有对应 `R:`。非草稿且无匹配时，确认 `ZR233` 位于 `R:` 后将其作为回退，并明确这是回退而不是所有权证据。草稿默认不更新审查人，除非用户明确要求。

默认仅新增：保留全部现有人类和机器人请求，把所有权目标与现有请求取并集，只新增缺失审查人；待删除审查人为空，除非用户明确要求删除或重新平衡。即使用户要求移除，也保留机器人，除非明确要求移除机器人。新请求中去掉 PR 作者和当前 GitHub 用户。

写入前查询当前状态与权限，并记录单 PR 演练：当前审查人、目标审查人、保留的人类和机器人、待新增、待删除、`F:`/`K:` 证据或回退、跳过原因。

```bash
gh api repos/<owner>/<repo>/pulls/<pr>/requested_reviewers
gh api repos/<owner>/<repo>/collaborators/<login>/permission
```

使用 REST `requested-reviewers` API，不用可能触发 Projects classic 问题的 `gh pr edit`。默认仅新增时不调用 DELETE：

```bash
printf '%s\n' '{"reviewers":["<login1>","<login2>"]}' |
  gh api -X POST repos/<owner>/<repo>/pulls/<pr>/requested_reviewers --input -
```

仅在用户明确要求删除或重新平衡时：

```bash
printf '%s\n' '{"reviewers":["<login>"]}' |
  gh api -X DELETE repos/<owner>/<repo>/pulls/<pr>/requested_reviewers --input -
```

分配后重新查询确认。GitHub 拒绝审查人时记录账号和精确 API 或权限错误，禁止静默换成允许列表外的人。最终向用户汇报匹配的 MAINTAINERS 项、已请求、已存在、已保留、已跳过、被拒绝、`ZR233` 回退、权限或 API 限制，以及审查人步骤是否只修改 GitHub 元数据。

## 清理

提交审查或明确不提交审查后：

- 删除无改动的审查或冲突工作树，并从主仓库运行 `git worktree prune`；
- 删除审查请求载荷、GraphQL 查询、评论、日志、冲突说明等临时文件，除非用户要求保留；
- 工作树有未提交的冲突修复、需要保留的诊断或用户改动时不得删除，向用户报告路径和原因；
- 确认主工作树未被审查流程修改；
- 清理后在同一个任务清单工具做最终审计，汇报已完成、不适用、阻塞和未完成项目。
