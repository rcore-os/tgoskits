---
name: review-single-pr
description: 审查本 tgoskits 仓库中一个指定的 GitHub Pull Request。适用于用户指定 PR 编号或 URL，要求审查、复审、对照 Linux/POSIX/RFC/VirtIO 语义、检查重复实现或相关开放 PR、建立并关闭 PR 专属审查清单、验证测试位置与发现和执行链路、在 CI 通过时仍本地运行新增或变更的 StarryOS/ArceOS 应用、修复安全的合并冲突、执行针对性验证、提交中文行内评论、批准或请求修改，以及审查后依据 .github/MAINTAINERS.md 推荐并分配审查人。
---

# 审查单个 PR

## 规范性要求

本 skill 是强制性审查规范，不是建议清单。触发后，必须完整阅读本文件，再作出审查结论；除非更高优先级指令冲突，否则执行所有适用要求。

判断代码质量、可维护性或可合入状态前，必须完整阅读 `docs/guideline/code-quality.md`。PR 新增或扩展用户可见行为、共享或公共接口、crate、子系统、平台或硬件能力时，必须完整阅读 `docs/guideline/feature-development.md`；触发条件按语义判断，不按标题判断。仅在语义适用时读取其他领域规范。任何改动或声明若影响 StarryOS syscall/Linux ABI，包括 task、VFS、namespace、signal、socket、credential、memory-management 等间接 helper，必须完整阅读 `docs/guideline/starry_syscall.md`。不适用时，在审查清单中记录具体理由。上下文被压缩、从摘要恢复或无法确信记得规范时，重新完整阅读，不能依赖记忆或旧的局部阅读。

没有完整阅读本 skill 和所有适用规范时，不得提交 `APPROVE`、`REQUEST_CHANGES`、no-submit 总结或任何面向 PR 的评论。规则重叠时采用更严格者；跳过要求时必须记录具体理由和证据。

## 审查清单门禁

详细判断前，只读取足以识别审查范围的当前 head 元数据、PR body、变更路径、commit 和语义范围，然后完整读取本 skill、仓库指令、必读规范以及规划验证所需的应用文档或 runbook。

完成这些读取后，必须立即通过可用的 todo/plan 工具创建用户可见、PR 专属的完整清单，并等待调用成功。持续使用同一个工具，最多一个项目为进行中；发现新范围时先追加清单再调查。工具返回空结果但未报错时视为成功。只有工具不可用或确认失败时才改用可见 Markdown checklist，并说明原因。

每个清单项必须写明具体 surface 和预期 evidence，覆盖：当前 head intake、既有 review threads、CI、worktree、必要时的冲突、每个受影响模块及 review lens、代码质量基线、feature-development 适用性、领域语义、测试的位置/构建/发现/选择/执行、重复与重叠分析、精确验证命令、阻塞 finding 与评论、head 刷新、审查提交、reviewer 分配和清理。每个受影响应用、环境准备、架构/运行命令以及每个新增或迁移测试都要有独立项目。禁止使用“review code”“run tests”之类泛化项目。

提交任何审查结论前逐项审计，只能以“有证据地完成”“给出具体理由的不适用”或“有证据的阻塞结论”关闭。阻塞结论会完成调查项，但必须进入中文审查文本和最终决定。任何必需项仍 pending、不可验证或缺少证据时禁止 `APPROVE`；若缺口由 PR 引入则提交 `REQUEST_CHANGES`，若外部审查系统限制阻止完成则明确 no-submit。审查提交、reviewer 分配和清理后，再做一次最终清单审计并向用户汇报完成项、不适用项、阻塞项和未完成项。

## 离线基准模式

仅当以精确参数 `offline-benchmark` 调用，且仓库存在 `.agent-review-context/reviewer.md` 时启用。否则执行正常在线流程。

以 `bench-base..HEAD` 为唯一被审变更。完整阅读本 skill、`AGENTS.md`、`docs/guideline/code-quality.md`、按需读取 `docs/guideline/feature-development.md` 和领域规范，并读取离线 contract 与输出 schema。应用本 skill 的审查重点、测试质量、阻塞 finding、硬件/ABI、安全/健全性、可维护性和文档要求。

离线环境没有真实 PR：PR 元数据、review threads、远端 CI、开放 PR 搜索、worktree、冲突修复、联网语义研究、命令验证、GitHub 提交、reviewer 分配和远端清理均标为不适用。禁止推断 PR 编号、访问仓库外路径或网络、修改文件、创建 commit/branch、运行 build/test。只使用只读仓库检查和 harness 允许的 Git 历史/diff 命令。

只返回 `.agent-review-context/review.schema.json` 要求的 JSON。finding 必须由 `bench-base..HEAD` 引入并锚定 `HEAD` 侧变更行；没有问题时返回空 `findings`。禁止提交或起草任何面向 GitHub 的审查文本。仍须创建并审计清单；若无 todo 工具，在内部跟踪，不能破坏 JSON-only contract。

## 目标与工具优先级

只审查指定的一个 PR，在隔离 worktree 中完成代码分析和本地验证；同时判断它是否重复 base 已有功能、与其他开放 PR 重叠、冲突或已被取代。正常结果是没有阻塞问题时 `APPROVE`，存在正确性、规范、重复、测试或 CI 覆盖问题时以中文行内评论提交 `REQUEST_CHANGES`。审查完成后，仅在仍需领域跟进时依据 `.github/MAINTAINERS.md` 分配合适的人类 reviewer。

本 skill 是 `review-open-prs` 的单 PR 权威流程。不要完整审查所有开放 PR，但必须读取足够的相关 PR 上下文来分类重复和重叠。

GitHub 操作优先遵循系统 skill：

- `github:github`：仓库定位、PR 元数据、patch、评论、label、reaction 和 connector-first 行为；
- `github:gh-address-comments`：未解决 thread、requested changes、行内上下文、锚点和 thread resolution；
- `github:gh-fix-ci`：失败的 GitHub Actions check 和日志。

优先使用 GitHub MCP/connector 获取结构化数据，本地 `git` 用于 fetch、worktree、diff 和验证；只有 connector 无法满足当前分支发现、GraphQL thread、Actions 日志或带锚点提交等需求时才使用 `gh`。

## PR 信息收集

1. 通过 `github:github` 获取仓库身份、当前用户、PR 编号/URL、title、body、author、base/head ref、`headRefOid`、draft、merge state、changed files、patch、commit、既有 review/comment 和 checks。
2. PR 作者是当前 GitHub 用户时，提交正式审查前先征询用户。
3. 除非用户明确排除，否则包含 draft PR。
4. 创建 worktree 前确保 connector 状态和本地 checkout 对齐。

connector 缺少必要数据时才回退：

```bash
gh auth status
gh repo view --json nameWithOwner,defaultBranchRef,url
gh pr view <pr> --json number,title,body,author,baseRefName,headRefName,headRefOid,headRepositoryOwner,isDraft,mergeStateStatus,maintainerCanModify,reviewDecision,url,commits
gh pr diff <pr> --patch --color=never
gh pr checks <pr> --watch=false
gh api --paginate "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
gh api --paginate "repos/<owner>/<repo>/pulls/<pr>/files?per_page=100"
```

## 审查讨论与 CI

涉及既有 requested changes、未解决讨论、行内位置或 resolution 状态时，遵循 `github:gh-address-comments`。扁平 comment 列表不能代表完整 thread 状态。需要时使用带分页的完整 GraphQL 查询：

```bash
gh api graphql --paginate \
  -F owner="$owner" -F repo="$repo" -F number="$pr" \
  -f query='query($owner:String!,$repo:String!,$number:Int!,$endCursor:String){repository(owner:$owner,name:$repo){pullRequest(number:$number){reviewThreads(first:100,after:$endCursor){nodes{id isResolved isOutdated path line diffSide comments(first:100){nodes{author{login} body createdAt}pageInfo{hasNextPage endCursor}}}pageInfo{hasNextPage endCursor}}}}}'
```

若任一 thread 的 `comments.pageInfo.hasNextPage=true`，再以该 thread 的 `id` 分页取得剩余评论，不能把前 100 条当作完整讨论：

```bash
gh api graphql --paginate \
  -F threadId="$thread_id" \
  -f query='query($threadId:ID!,$endCursor:String){node(id:$threadId){... on PullRequestReviewThread{comments(first:100,after:$endCursor){nodes{author{login} body createdAt}pageInfo{hasNextPage endCursor}}}}}'
```

检查所有未解决 thread。具体问题已在当前 head 修复时 resolve；修复不完整、测试未接入 runner 或评论仍有效时保持 open。resolve 后重新查询并确认 `isResolved=true`：

```bash
thread_id='<thread-id>'
gh api graphql \
  -f query='mutation($threadId:ID!){resolveReviewThread(input:{threadId:$threadId}){thread{id isResolved}}}' \
  -f threadId="$thread_id"
```

CI 必须绑定当前精确 `headRefOid`/`head.sha`。优先查询 `statusCheckRollup`、check suites 和 check runs；REST fallback 也必须使用同一 SHA。不能单独用 classic `GET /repos/<owner>/<repo>/commits/<sha>/status` 判断 Actions 状态，因为它可能显示 `pending` 且 `statuses` 为空，而 Actions 已结束。

```bash
gh api --paginate "repos/<owner>/<repo>/commits/<head-sha>/check-runs?per_page=100"
gh api --paginate "repos/<owner>/<repo>/actions/runs?head_sha=<head-sha>&per_page=100"
gh api --paginate "repos/<owner>/<repo>/actions/runs/<run-id>/jobs?per_page=100"
```

区分预期的 matrix/path-filter `skipped` 与整个相关 workflow 未运行。本仓库互斥的 `run_host`/`run_container`、分支限制发布任务或 path-filter job 可以 `skipped`，其成功 sibling 足以说明 workflow 运行。以 `success=N, skipped=M, failure=0` 汇报，并命名关键 check。只有变更 surface 应由该 check 覆盖、path filter 跳过必需覆盖或所有相关 check 均被跳过时，才把 `skipped` 视为可疑。

提交审查前检查每个失败、取消、缺失或可疑 check 的日志，并分类：PR-related、unrelated 或 unclear。

- PR-related：失败 job 覆盖本 PR 的文件、crate、case、命令、平台或行为；在 PR head 可复现而 base 不失败；或新增/修改的测试、配置、workflow 导致失败、hang、skip、timeout。必须 `REQUEST_CHANGES`，说明 check、失败模式、归因和修复方向。
- unrelated：必须有具体证据，例如变更范围外、base 同样失败、已知 flake/基础设施问题或已有 issue。用 workflow/job、特征错误、runner/platform、case/命令等多个关键词搜索并检查候选 issue；更新合适的现有 issue，或在确无匹配时创建唯一 issue，并在审查正文链接它。
- unclear：合理检查后因果仍不清楚时，禁止仅凭 CI 批准；根据证据请求修改，或在用户只要求调查时明确 no-submit blocker。

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

日志下载为空时不能推断通过或无关；用 `gh pr checks` 和 `gh run view <run-id> --json headSha,jobs` 确认当前 head、失败 job、conclusion 和 step。

## 工作树

fetch PR 和 base，然后在 detached worktree 审查：

```bash
repo_root="$(git rev-parse --show-toplevel)"
repo_parent="$(dirname "$repo_root")"
review_wt="$repo_parent/$(basename "$repo_root")-review-pr<pr>"
git fetch origin '+refs/pull/<pr>/head:refs/remotes/origin/pr/<pr>' '+refs/heads/*:refs/remotes/origin/*'
git worktree add --detach "$review_wt" origin/pr/<pr>
```

已有 worktree 仅在 clean 且位于当前 PR head 时复用：

```bash
git -C "$review_wt" status --short
git -C "$review_wt" rev-parse HEAD
git rev-parse refs/remotes/origin/pr/<pr>
```

stale 且 clean 时无损更新；有本地改动时新建 worktree 或询问用户。禁止修改或回滚用户主 worktree。并行审查不同 PR 时必须使用不同 worktree；同一 checkout 内不得并发运行多个 StarryOS QEMU case。

## 合并冲突

仅在用户明确要求，或审查没有其他 blocker、本应 `APPROVE` 且当前 `mergeStateStatus=DIRTY`、`maintainerCanModify=true` 时修复。修复并 push、重新验证新 head 前不得批准。

`reviewDecision=APPROVED` 才代表当前 aggregate approval。历史 `APPROVED` review 只能作为上下文；aggregate approval 为空、为 `CHANGES_REQUESTED`，或仍有 unresolved threads 时，冲突修复只能做 no-submit dry run，除非用户明确要求 push 修复。

先刷新 PR 元数据、review 和远端 head。`mergeStateStatus=UNKNOWN` 时等待并重查。`DIRTY` 且 `maintainerCanModify=false` 时不得修复：用户明确要求处理冲突时提交 `REQUEST_CHANGES`，说明作者需合并/rebase 最新 base 并建议启用 Allow edits by maintainers；否则在正文或总结中记录限制。`DIRTY` 且可修改时使用独立 conflict worktree，并确认 fork branch 仍等于 `headRefOid`。

```bash
gh pr view <pr> --json number,baseRefName,headRefName,headRepositoryOwner,headRefOid,mergeStateStatus,maintainerCanModify,reviewDecision,reviews
gh api --paginate "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
git fetch origin '+refs/pull/<pr>/head:refs/remotes/origin/pr/<pr>' '+refs/heads/<base>:refs/remotes/origin/<base>'
git ls-remote "https://github.com/<head-owner>/<repo>.git" "refs/heads/<headRefName>"
git worktree add --detach "$conflict_wt" origin/pr/<pr>
git -C "$conflict_wt" merge --no-ff --no-commit "origin/<base>"
git -C "$conflict_wt" diff --name-only --diff-filter=U
```

detached conflict worktree 中，stage 2/ours 是 PR，stage 3/theirs 是 base；不清楚时使用 `git show :1:<path>`、`:2:`、`:3:`。按 PR 意图和当前 base 语义解决，禁止简单保留两边或复活 base 已替换的 API。PR 837 是参照：保留 `/proc/kallsyms` 功能，但适配 `SeqObject` 与 `SpecialFsFile::new_regular_with_perm`，并同时保留 `ktracepoint`/`ksym`、`.tracepoint`/`.kallsyms` 等独立改动，而不是恢复旧 `SeqFile`。

提交修复前运行格式化、冲突标记扫描、diff hygiene 和针对性验证。解决 `Cargo.lock` 冲突时先处理其他文件，再由 Cargo 重新生成，禁止手工拼接。

```bash
cargo fmt
rg -n '<<<<<<<|=======|>>>>>>>' <conflicted-files>
git -C "$conflict_wt" diff --check
<targeted cargo xtask/cargo test/cargo clippy commands>
git -C "$conflict_wt" add <resolved-files>
git -C "$conflict_wt" commit
```

push 前确认 merge commit 第一父节点仍是当前 `headRefOid`，并再次 `git ls-remote`。远端变化时停止并重新审查。只能 normal push，禁止 force push：

```bash
git push https://github.com/<head-owner>/<repo>.git HEAD:<headRefName>
```

push 后刷新 PR、更新 review worktree，并重跑支持批准的验证。冲突消失后，`BLOCKED`/`UNSTABLE` 仍可能由 CI 或 review 状态导致，不能仅据此判定 conflict repair 失败。只做冲突 dry run 时不得 push 或提交 review；记录 PR、approval 状态、冲突文件、语义解法、验证结果和未修改 GitHub 的事实，然后清理 worktree。

## 审查重点

按 PR 意图、当前 base、项目既有模式和适用外部语义理解完整实现逻辑：

- syscall、process/session/signal、filesystem errno、socket、IPv4/IPv6、`/proc` 对照 POSIX/Linux；
- 网络行为对照 RFC/Linux，包括 IPv6 NDP、IPv4-mapped IPv6、dual-stack、route/listen conflict 和 errno；
- driver 改动检查 VirtIO、PCI、DMA、MMIO、IRQ 和 ownership；
- Axvisor 配置检查 `entry_point`、`kernel_load_addr`、`memory_regions`、`map_type` 和 guest image layout；
- Starry 测试改动应用 `starry-test-suit`；portable driver 或 OS glue 改动应用 `cross-kernel-driver`。

影响 StarryOS syscall/Linux ABI 时，按 `docs/guideline/starry_syscall.md` 的证据层级追踪间接 helper 到每个受影响 syscall entry；行为随版本变化时记录对照的 Linux 版本或 commit。

### 新功能设计门禁

新增或扩展功能时，按 `docs/guideline/feature-development.md` 分类 local/shared/high risk，并在清单记录分类和证据位置。按以下顺序审查：必要性、重复性、语义与 prior art、替代方案、整体架构/API、实现、验证与交付。

必须核对具体问题、目标用户/调用方、真实场景、成功标准、non-goals、仓库内部研究、适用的权威外部研究、现实替代方案和不实现成本。high-risk 功能必须有可独立审查的设计材料，覆盖适用的 ownership、dependency、compatibility、migration、rollback、observability、performance 和 security。先提交重大设计 blocker，再处理低层 polish。测试通过不能替代“为什么项目需要它、为什么优于复用/扩展、为什么复杂度现在必要”的解释。

### 审查视角与问题纪律

审查采用 recall-first：优先找全 changed surface 的真实缺陷，不为简短而漏报，也不臆造问题。对可疑缺陷构造具体 input、interleaving、device state、guest config 或 test path；若场景不可能则说明原因。

除非变更显然不涉及，否则应用五类 lens：

- `Maintainability`：流程、commit hygiene、范围、crate/module 边界、命名、可见性、注释和可理解性；
- `Correctness`：正常、错误、并发、hot path，off-by-one、可达 `unwrap`/`expect`/`panic`、overflow、错误 predicate、guard、wakeup 和 resource leak；
- `Security/Soundness`：`unsafe` contract、pointer provenance、aliasing、user memory、trust boundary、privilege、TOCTOU、use-after-free；
- `Hardware/ABI`：assembly、target JSON、trap/context、SMP/boot、MMIO/DMA/IRQ、cache/coherency、VirtIO/PCI、device tree/config、calling/alignment；
- `Documentation/User-Facing Compatibility`：文档、runbook、app workflow、test-suit guide、兼容性说明和对用户可见行为。

每个 finding 必须包含：`grounding`、`severity`、具体 `problem` 与触发场景、`fix direction`、`evidence`。同一根因避免在每个 lens 重复；多个症状可分别锚定，但只解释一次共享修复。提交前复核所有承重前提、引用代码和权威外部来源；不确定性必须明确，前提错误的 finding 必须撤回。

### 测试与行为门禁

bug 修复必须有 deterministic regression/reproduction：未修复实现必然失败，修复后同一测试通过；除非有具体证据说明环境不可能做到，否则缺少 red/green 证明即阻塞。raw syscall 修复优先直接覆盖 `syscall(SYS_...)`，避免 libc wrapper 掩盖返回值或 errno。

新增行为、语义变更、bug 修复或新暴露路径必须在正确项目层级有测试。不能只看到测试文件：必须验证 runner 能发现、构建/安装、选择、执行，并且回归时会失败。错放、孤立、manual-only、opt-in 或被 CI 静默跳过的测试按缺失处理。

StarryOS app-support 分层：

- operator-facing smoke、demo、rootfs、board/QEMU script、长运行或 opt-in workflow 放 `apps/starry/<app-or-scenario>/`；
- kernel ABI、syscall、filesystem、process、networking 或 bugfix 语义覆盖放 `test-suit/starryos/<case>` 或既有 grouped wrapper；
- syscall 变化必须有直接 test-suit regression；app smoke 不足以证明 syscall；
- app 暴露的 kernel bug 尽量提取无需完整 app 的 test-suit regression，app scenario 保留为 integration evidence。

每个新增、变更或 PR 明确声明支持的 StarryOS/ArceOS app，都必须建立独立 setup/runtime todo，并在当前 head 按文档本地实跑。远端 CI 不能替代。generic 改动至少跑一个最高风险 claimed architecture；architecture-specific 改动跑每个新增/变更架构。文档无法准备环境、需要未记录 workaround、外部依赖不稳定、硬件/credential/permission/service/host capability 不可用，或只能跑比声明更窄 target，均 `REQUEST_CHANGES`。

禁止 test-shaped/fake fix、hard-coded special case、fake state、no-op compatibility shim 或未实现真实语义的逻辑。success-path 测试遇到 `ENOMEM`/`EAGAIN` 等意外失败时不得静默 return；合法 skip 必须打印明确 marker 并解释原因。禁止删减 case、架构、放宽 `success_regex`/`fail_regex`、把失败变成 skip/timeout、修改 path filter 跳过相关覆盖，或把 CI 覆盖移到 manual-only，除非有等价或更强且已验证的替代。

Starry QEMU 失败必须传播到 `cargo xtask starry test qemu ...`：wrapper 必须在命令后立即保存 `$?`，失败时打印 `STARRY_GROUPED_TEST_FAILED` 或配置 marker，不得再打印 all-passed marker，并让外层命令失败。`success_regex`/`fail_regex` 必须可靠分类。当前 `qemu/system` grouped C subcase 的 `CMakeLists.txt` 与 `src/` 必须直接位于 `system/<subcase>/`；`system/<subcase>/c/` 默认阻塞，除非同时更新 root CMake、runner discovery、guide 和 rule tests 并验证。

## 可发布 Cargo 补丁策略

PR 触及 `Cargo.toml`、`Cargo.lock`、已提交的 `.cargo/config`/`.cargo/config.toml`、dependency metadata、重复版本、第三方 API 或跨依赖类型边界时，检查所有 `[patch]` 和变更的 dependency source。按来源是否能由本仓库或 crates.io 重现、workspace 是否可发布来判断；存在 `[patch.crates-io]` 本身不阻塞。

允许但必须通过解析与发布检查的来源：

- 相对于声明它的 manifest/config 解析并 normalize 后仍位于当前仓库内的 `path`；
- crates.io 已发布的精确版本，包括普通依赖中的 `version = "=1.2.3"`，以及用该版本替代其他 source 的 registry-backed patch。

以下情况阻塞：任意 `git`、绝对 `path`、逃逸仓库的相对路径、非 crates.io registry；metadata 未解析到预期 package/version/source；请求版本未发布；依赖统一破坏 API 或类型语义；完整 workspace publish dry-run 失败。

发布 package 可使用 `{ path = "...", version = "..." }`，打包时 Cargo 使用 crates.io version requirement；只有 `path` 的普通依赖对需要发布的 package 是阻塞项。root `[patch]` 中的仓库相对路径自身不要求 version fallback，但发布 package 的普通 dependency declaration 仍要求。

patch 若只为掩盖 dependency-owned 与 workspace-owned 类型不一致，优先正常 crates.io resolution 和显式边界：使用依赖公开类型；在边界添加 crate-private adapter；使用 `.map_err(...)`、`TryFrom`、wrapper newtype 或 extension trait；未知 errno 提供明确 fallback。root `[patch.crates-io] ax-errno = { path = "components/axerrno" }` 的来源形态允许，并可在 metadata 与完整发布 dry-run 证明时统一 release graph；若目的只是让 `kbpf-basic` 错误与另一份本地错误类型隐式互换，则保留 `kbpf_basic::BpfError`/`BpfResult` 到 `LinuxError`/`AxError`/`AxResult` 的显式转换。

## 重复与重叠分析

每个 PR 必做。先建立 intent fingerprint：title/body/issue/commit、changed crate/module/test/config/CI/generated asset、public API/syscall/errno/protocol/device/runner/feature，以及语义 claim（feature、fix、coverage、refactor、config、CI、dependency metadata）。

先查当前 base 是否已有等价或更新实现，再用多个 fingerprint 关键词搜索开放 PR；不能只搜标题。读取候选的 intent、文件和 diff 后分类：

- `duplicate`：同一问题或同一 API/test/config，无实质差异；
- `partial-overlap`：同一 surface，但互补、可排序或可拆分；
- `conflict-risk`：修改同一 contract、runner、generated asset 或 ABI，存在 merge/semantic 冲突；
- `superseded`：base 或其他 PR 更完整、更符合项目方向；
- `unrelated-after-inspection`：关键词命中但审阅后无关。

```bash
git grep -n -E '<relevant symbols|paths|commands>' origin/<base> -- <likely paths>
git log --oneline --decorate -- <likely paths>
gh pr list --state open --limit 200 --search '<symbol OR path OR issue keyword>'
gh pr view <related-pr> --json number,title,body,author,baseRefName,headRefName,isDraft,updatedAt,files,commits
gh pr diff <related-pr> --patch --color=never
git diff --name-only origin/<base>...origin/pr/<related-pr>
```

依赖另一 PR 先落地时，在 body/review 明确依赖前不得批准。`duplicate`/`superseded` 应请求修改或中性说明应优先采用的 base/PR。使用 `git diff origin/<base>...origin/pr/<pr>` 查看 PR patch；只有检查 stale-branch effect 时才用 `..`。用户要求关闭时，先 `gh pr comment <pr> --body-file comment.md`，再 `gh pr close <pr>`。

## 验证

验证必须匹配 changed surface，优先项目 `cargo xtask`：

```bash
cargo fmt --check
cargo xtask clippy --package <crate>
cargo clippy --manifest-path <path>/Cargo.toml --all-features -- -D warnings
cargo xtask starry test qemu --arch <arch> -c <case>
cargo xtask axvisor build ... --vmconfigs <config>
```

特殊配置无法由 xtask 覆盖时，先检查 xtask help/source，再用参数完全匹配的 native Cargo。记录精确命令与失败。

dependency metadata 变更必须扫描 patch、检查 metadata/tree 并执行完整 workspace publish dry-run：

```bash
rg --hidden -n '^\s*\[patch(?:\.|\])' -g 'Cargo.toml' -g '**/.cargo/config' -g '**/.cargo/config.toml' .
cargo metadata --locked --format-version=1 | jq -r '.packages[] | [.name,.version,.source,.manifest_path] | @tsv' | rg '<affected-crate>'
cargo tree -p <affected-package> | rg '<affected-crate>|<boundary-crate>'
cargo publish --workspace --dry-run --no-verify
```

相对 path 按声明文件解析并确认 normalize 后仍在 repo root 下；精确 crates.io replacement 必须在 metadata 中显示 crates.io registry source 和精确版本。新增、变更或依赖 patch，或修改可发布 workspace package 的 source 时，全 workspace 打包/解析 dry-run 是硬门槛；涉及 workspace 发布顺序或 path-to-registry rewriting 时，单 package dry-run 不能替代。`--no-verify` 会跳过 package verification build，因此该命令不能代替前面的针对性 build/clippy/runtime 验证。

每个受影响 app 必须严格按 PR body 或变更文档执行：

1. 分别列出环境准备、架构、runtime command 和可观察 postcondition。
2. 文档必须覆盖 package、toolchain、rootfs、permission、hardware、credential、network service、env var、asset、参数和 readiness check；只能引用完整覆盖该 app 的 canonical section。
3. 不使用本地知识补充未记录命令，不依赖未说明的机器状态。
4. 在当前 head 运行真实 `cargo xtask starry app qemu ...`、`cargo xtask starry test qemu ...`、`cargo xtask arceos test qemu ...` 或文档 wrapper。
5. 验证 guest marker、app output、log、symbolized block、package artifact 等真实结果；退出 0 但未执行行为不算通过。
6. `tmp/axbuild/rootfs` 为空时仍尝试文档中的 rootfs/test 命令，让 xtask 自动下载；失败则记录并 `REQUEST_CHANGES`。
7. 同一 worktree 内一次只运行一个 Starry QEMU case。

同样的 executable-workflow 门禁适用于 ArceOS app、`apps/**` demo，以及准备、启动、检查、symbolize、解析日志、打包或操作 StarryOS/ArceOS 应用的 QEMU wrapper、rootfs/app preparation tool、symbolizer、log parser 和 packaging helper。即使 tool-only 改动未声明具体 app，只要 CI 没有执行精确 workflow，就必须本地运行真实流程，不能只验证 syntax、文档、`--help`、解析或 build。tool-only 场景若确因硬件、credential、service 或 host capability 不可用，记录限制并要求受控 fallback 或可复现验证；一旦涉及具体 app，仍应用更严格的逐 app 门禁，环境不可用不能豁免。

grouped QEMU 新增或迁移测试必须核对 `test_commands` discovery/install、`/usr/bin/<test>`、`status=127`、subcase selection、feature gate 和 regex。至少取得以下一种证据：当前 head 本地运行、当前 head CI 明确显示该 case/binary 执行、或 deterministic build/discovery check。aggregate CI 通过不足以证明未被跳过。检查 shell wrapper 失败分支和 grouped bugfix assertion，不能只跑成功路径。

对每个新增或迁移测试，不限于 grouped QEMU，都必须写明实际执行它的 runner command，并取得以下至少一种 current-head 证据：本地执行；CI 日志明确显示具体 case/subcase/binary 执行；或 deterministic build/discovery check 证明 runner 一定到达该测试。宽泛的 aggregate CI pass 不能证明未被 path layout、filter、install rule、subcase selection 或 feature gate 跳过。

app-support 同时包含 syscall/kernel bugfix 时，app workflow 与对应 `cargo xtask starry test qemu` 都要运行并分别汇报。没有测试变更时，若 PR body/commit 声称 QEMU、host unit、xtask、clippy、script、emulator 等 non-board validation，必须复跑并核对 command、target、output 和 pass condition；不可复现、静默 skip、target 更窄或失败时请求修改。既无测试又无可复现 non-board validation 时禁止批准。board-only 证据不能单独满足此门槛，除非用户明确限定审查范围。

remote CI 是必需证据但不是唯一证据；没有 check 不等于通过，CI 通过也不能替代本地分析和针对性运行。

## 阻塞问题

除非有明确证据表明不阻塞，否则以下情况阻塞：

- 与 POSIX/Linux/RFC/VirtIO 语义不符；
- 新功能缺少问题、用户/调用方、成功标准、non-goals、内部重复搜索、适用权威研究或现实替代方案；
- high-risk 功能缺少可独立审查设计或合格领域 reviewer；
- 跨层 shortcut、hard-coded special path、重复真相源、fake success、silent fallback、无当前 consumer 的投机 API/config/extension；
- targeted test、format、clippy 或 PR-related CI 失败；
- 当前 head 的 StarryOS/ArceOS app/QEMU case 按文档失败，或失败未传播到 xtask；
- app/QEMU 声称仅验证 discovery、TOML parsing、旧 head 或他人结果；
- 任何受影响 app 未完成当前 head 的文档化 setup/runtime，或文档缺环境、命令、参数、readiness；
- app 需要不可用硬件/credential/permission/service/host capability 或未记录 workaround；
- 新行为/语义/bugfix 缺测试，或测试错位、未发现、未构建/安装、未选择、未直接覆盖 ABI；
- coverage 因 layout、path filter、feature gate、subcase、install rule 或 manual-only placement 被跳过；
- 无测试变更且无可复现 non-board validation，或声明的验证不可复现/不匹配；
- `success_regex`/`fail_regex` 不能可靠分类；
- bugfix 缺少必然 red/green 的 regression/reproduction，且未证明不可能；
- Cargo patch 使用 `git`、绝对 path、仓库外 path、非 crates.io registry，普通可发布 dependency 只有 path，请求 crates.io 版本不存在，解析到非预期 package/version/source，或完整 workspace publish dry-run 失败；
- merge conflict 未解决，修复复活过时 base API，或 push 后未重新验证；
- app workflow/test-suit 语义覆盖层级错误；
- test-only/fake fix 未实现真实行为；
- buffer、DMA memory、queue token、IRQ ownership 泄漏、过早释放或跨错抽象层；
- CI hang、timeout、skip 新覆盖，或削弱既有 case/architecture/regex/path-filter/正常回归；
- 重复 base、削弱已有实现、与开放 PR 冲突或已被取代；
- 无法解释与候选相关 PR 的差异；
- 必需 todo 仍 pending、不可验证或缺少证据/具体不适用理由。

所有 GitHub review text（行内评论、body、reply）必须中文、中性、项目导向。每个 blocker 都写明 `grounding`、`severity`、具体问题与场景、`evidence`、修复方向。优先锚定当前 PR diff 的 RIGHT side changed line；提交前确认 `line` 存在，否则移动到最近能证明问题的 changed line 或放入 body。

## 提交审查

提交前用同一个 todo 工具逐项审计。任何必需项 pending、不可验证或无证据时不得 `APPROVE`。PR 导致的测试证据缺失、环境准备失败或 app runtime 失败进入 `REQUEST_CHANGES`；外部系统阻止提交时明确 no-submit。

通过 connector 确认 PR head SHA 未变化；fallback：

```bash
gh pr view <pr> --json number,headRefOid,reviewDecision
```

head 变化时 fetch 新 head、更新 worktree、在当前 changed line 复核每个 finding，并重跑支撑结论的验证。

优先用 connector 一次提交 event 和带锚点评论；connector 无法保持锚点时用 REST：

```bash
gh api --method POST repos/<owner>/<repo>/pulls/<pr>/reviews --input review.json
```

payload 必须使用当前 `headRefOid`、`side=RIGHT`；有任何 blocker 用 `REQUEST_CHANGES`，无 blocker 才用 `APPROVE`：

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

禁止提交针对旧 head 的 finding。提交后重新查询；若期间出现新 commit，仅在 blocker 对新 head 仍成立时提交 follow-up。

中文 review body 必须覆盖：PR 改动；feature-development 适用性、risk 和 design location；新功能的问题/用户/标准/non-goals/研究/替代/取舍；实现逻辑和项目语义；验证命令与结果；测试要求、位置、发现/选择/执行证据；每个 app 的 head、setup source、准备命令、runtime、arch 和 postcondition/失败；todo 审计；无测试时复核的 claim；CI 状态、无关失败证据和 tracking issue；duplicate/overlap 分类；冲突处理；PR-related CI 失败与修复方向；bugfix red/green；resolved/open threads；未实现或后续项；环境限制。不能只写“tests pass”。

```bash
gh pr view <pr> --json number,reviewDecision,latestReviews
```

## 推荐审查人分配

仅在审查提交后仍需领域跟进时请求 reviewer。读取 `.github/MAINTAINERS.md`；它是本地来源真相和自动人类 reviewer 严格 allowlist。只有 `R:` 可自动请求；`M:` 只是 ownership metadata，除非同一 login 也在 `R:`。不得推断或请求 allowlist 外的人类 reviewer。

用 PR title/body、changed path、API、test、validation、finding、crate/config/feature 和 diff-visible identifier 匹配 `F:`/`K:`。多个 section 命中时请求所有对应 `R:`。非 draft 无匹配时，确认 `ZR233` 位于 `R:` 后将其作为 fallback，并明确这是 fallback 而非 ownership evidence。draft 默认不更新 reviewer，除非用户明确要求。

默认 add-only：保留全部现有人类和 bot request，把 ownership target 与现有 request 取并集，只新增缺失 reviewer；`reviewers to remove` 为空，除非用户明确要求删除/rebalance。即使用户要求移除，也保留 bot，除非明确要求移除 bot。新请求中去掉 PR author 和当前 GitHub 用户。

写入前查询当前状态与权限，并记录单 PR dry run：current、target、preserved human/bot、to add、to remove、`F:`/`K:` 证据或 fallback、skip reason。

```bash
gh api repos/<owner>/<repo>/pulls/<pr>/requested_reviewers
gh api repos/<owner>/<repo>/collaborators/<login>/permission
```

使用 REST requested-reviewers API，不用可能触发 Projects classic 问题的 `gh pr edit`。默认 add-only 不调用 DELETE：

```bash
printf '%s\n' '{"reviewers":["<login1>","<login2>"]}' |
  gh api -X POST repos/<owner>/<repo>/pulls/<pr>/requested_reviewers --input -
```

仅在用户明确要求删除/rebalance 时：

```bash
printf '%s\n' '{"reviewers":["<login>"]}' |
  gh api -X DELETE repos/<owner>/<repo>/pulls/<pr>/requested_reviewers --input -
```

分配后重新查询确认。GitHub 拒绝 reviewer 时记录 login 和精确 API/permission 错误，禁止静默换成 allowlist 外的人。最终向用户汇报匹配的 MAINTAINERS 项、requested/already present/preserved/skipped/rejected、`ZR233` fallback、权限/API 限制，以及 reviewer 步骤是否只修改 GitHub metadata。

## 清理

审查提交或明确 no-submit 后：

- 删除 clean review/conflict worktree，并从主仓库运行 `git worktree prune`；
- 删除 review payload、GraphQL query、comment、log、conflict note 等临时文件，除非用户要求保留；
- worktree 有未提交 conflict repair、需要保留的诊断或用户改动时不得删除，向用户报告路径和原因；
- 确认主 worktree 未被审查流程修改；
- 清理后在同一个 todo 工具做最终审计，汇报 completed、not-applicable、blocking 和 unfinished。
