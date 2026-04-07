# TGOSKits 仓库管理指南

本文档介绍 TGOSKits 主仓库如何使用 Git Subtree 管理独立组件仓库，以及当前生效的双向同步方案。

## 1. 概述

### 1.1 项目定位

TGOSKits 是一个统一工作区仓库。它通过 Git Subtree 将多个独立组件仓库整合到主仓库中，既保留组件的独立开发能力，又支持在主仓库中做跨系统联调、统一测试和集中维护。

### 1.2 核心特性

- 统一工作区：在一个仓库里同时开发组件、OS 和平台相关代码
- 历史保留：Subtree 保留组件的提交历史，便于追溯问题来源
- 双向同步：支持主仓库推送改动到组件仓库，也支持组件仓库反向向主仓库发起同步 PR
- 配置显式：所有组件来源、路径和分支信息集中记录在 `scripts/repo/repos.csv`

### 1.3 仓库结构

当前仓库的实际结构大致如下：

```text
tgoskits/
├── components/                # subtree 管理的独立组件 crate
├── os/
│   ├── arceos/
│   ├── axvisor/
│   └── StarryOS/
├── platform/                  # 平台相关 crate
├── scripts/
│   └── repo/
│       ├── repo.py            # subtree 管理脚本
│       └── repos.csv          # 组件来源配置
├── .github/workflows/
│   └── push.yml               # 主仓库 -> 组件仓库 自动推送
└── docs/
```

需要特别注意：

- `components/` 并不是按 `Hypervisor/ArceOS/Starry` 建目录分层
- 大多数组件直接平铺在 `components/` 下
- 组件分类主要来自 `scripts/repo/repos.csv` 中的 `category` 字段，而不是目录层级

## 2. 分支管理

TGOSKits 主仓库采用三层层级的分支管理策略：`main` 作为稳定发布分支、`dev` 作为集成分支、以及开发者个人使用的功能分支。这套策略与第 6 节描述的双向自动同步机制紧密配合。

### 2.1 分支总览

```text
功能分支 (feature/*, fix/*, ...)
    │  开发者本地开发、自测
    │
    │  PR（禁止直接发到 main）
    ▼
  dev  集成分支
    │  实时汇聚开发功能、执行 CI 测试
    │  push.yml 自动推送 subtree 改动到组件仓库 dev
    │
    │  定期合并到 main（含分支保护）
    ▼
 main  稳定发布分支
    │  带有分支保护，每周合并来自 dev 的内容
    │  push.yml 自动推送 subtree 改动到组件仓库 release/main
    │  根据需要发布版本
    │  接收组件仓库反向同步 PR
    └──────────────────────────────────────
```

### 2.2 main 分支 — 稳定发布分支

`main` 分支是仓库的稳定基线，始终处于可发布状态。该分支带有分支保护规则（见下方），确保只有经过充分验证的代码才能进入。

核心规则：

- **每周合并**：定期从 `dev` 分支合并最新内容，合并前应确认 CI 测试通过
- **版本发布**：作为稳定发布线独立演进，由 `release.yml` 自动创建或更新面向 `main` 的版本 PR，并在 PR 合并后执行正式发布
- **分支保护**：禁止直接 push，所有代码变更必须通过 PR 进入
- **接收同步 PR**：组件仓库通过其 `push.yml` 向主仓库 `main` 发起的 subtree 同步 PR 也在此合并

### 2.3 dev 分支 — 集成分支

`dev` 分支是日常开发的主战场，所有功能开发和 bug 修复最终都汇聚到这里。

核心规则：

- **实时汇聚**：所有经 PR 验证的功能分支合并到 `dev`，确保开发进度持续集成
- **CI 测试**：`dev` 分支上的每次 push 都触发 CI 测试（`.github/workflows/test.yml` 等），确保代码质量
- **自动同步**：`dev` 分支上的 push 会触发 `push.yml`，自动将组件改动推送到各组件仓库的 `dev` 分支（详见第 6.1 节）
- **独立发布**：`dev` 也会由 `release.yml` 自动创建或更新面向 `dev` 的版本 PR，并在 PR 合并后执行正式发布
- **作为合并源**：`main` 分支仍可定期从 `dev` 合并，因此 `dev` 应保持可编译、可测试的状态

### 2.4 功能分支 — 开发者个人分支

开发者基于 `dev` 分支创建功能分支进行开发。功能分支的命名建议遵循以下约定：

| 类型 | 命名格式 | 示例 |
|------|----------|------|
| 新功能 | `feature/<描述>` | `feature/vm-pause-resume` |
| Bug 修复 | `fix/<描述>` | `fix/pl011-uart-overflow` |
| 重构 | `refactor/<描述>` | `refactor/axvm-crate-split` |
| 文档 | `docs/<描述>` | `docs/branch-policy` |
| 实验性 | `experiment/<描述>` | `experiment/riscv-smp` |

### 2.5 PR 规则与合并流程

所有代码变更必须通过 Pull Request 进入 `dev` 分支，以下是合并流程的关键约束：

```text
1. 开发者在功能分支上完成开发和自测
2. 向 dev 分支提交 PR（禁止直接向 main 提交 PR）
3. CI 自动运行测试矩阵
4. 代码评审通过、CI 绿色后合并到 dev
5. dev 积累一定周期后，定期合并到 main 并发布
```

**重要约束**：

| 规则 | 说明 |
|------|------|
| **禁止直接 PR 到 main** | 所有开发 PR 的目标分支必须是 `dev`，只有维护者在定期合并时才操作 `main` |
| **功能分支基于 dev** | 新功能分支应从 `dev` 创建，不要从 `main` 创建，以避免合并冲突 |
| **保持分支更新** | 功能分支开发周期较长时，应定期 rebase `dev` 以减少最终合并的冲突 |
| **PR 描述完整** | PR 应包含变更说明、测试方法和关联 issue |

### 2.6 与自动同步的关系

分支管理策略与第 6 节描述的双向自动同步机制紧密配合，具体关系如下：

- **主仓库 `dev` → 组件仓库 `dev`**：`push.yml` 在 `dev` 分支 push 时触发，自动推送 subtree 改动到所有组件仓库的 `dev`
- **主仓库 `main` → 组件仓库 `release/main`**：`push.yml` 在 `main` 分支 push 时触发，`arceos-org/*` 推到 `release`，其他仓库推到 `main`
- **组件仓库 `main` → 主仓库 `main`**：组件仓库的 `push.yml` 在组件仓库主线更新时触发，向主仓库 `main` 发起同步 PR
- **主仓库 `main/dev` 版本编排**：`release.yml` 在 `main` 和 `dev` 分支的 push 上运行，自动检测组件变动并创建或更新版本 PR
- **主仓库正式发布**：`main` 和 `dev` 上的版本 PR 合并后，`release-plz` 都会在主仓库创建 tag、GitHub Release，并将可发布 crate 发布到 crates.io

```text
功能分支 ──PR──► 主仓 dev ──push.yml──► 组件仓库 dev
                      │
                      ├─ 定期合并
                      ▼
                主仓 main ──push.yml──► 组件仓库 release 或 main
                      ▲
                      │ 组件仓库 push.yml（以 PR 方式）
组件仓库 main ─────────┘
```

## 3. 组件配置

### 3.1 `repos.csv`

Git Subtree 不像 Git Submodule 那样自带 `.gitmodules`。这意味着：

- Git 本身不会持久记录“某个目录对应哪个远程仓库”
- 临时 remote 在命令执行后会被清理
- 单靠 Git 命令无法完整恢复组件来源信息

因此，TGOSKits 使用 [repos.csv](/home/zcs/WORKSPACE/tgoskits/scripts/repo/repos.csv) 作为组件来源配置清单。

### 3.2 字段说明

`repos.csv` 的格式为：

```text
url,branch,target_dir,category,description
```

字段含义如下：

| 字段 | 必填 | 说明 | 示例 |
|------|:----:|------|------|
| `url` | 是 | 组件仓库 URL | `https://github.com/arceos-org/ax-cpu` |
| `branch` | 否 | 建议跟踪的分支；留空时由 `repo.py` 自动检测 | `dev` |
| `target_dir` | 是 | 组件在主仓库中的路径 | `components/axcpu` |
| `category` | 否 | 组件分类 | `ArceOS` |
| `description` | 否 | 备注描述 | `CPU abstraction component` |

### 3.3 当前组件分布

仓库中的组件大致分为以下几类：

| 分类 | 说明 |
|------|------|
| `Hypervisor` | 虚拟化相关组件 |
| `ArceOS` | ArceOS 基础组件、驱动和支撑库 |
| `Starry` | StarryOS 相关组件 |
| `OS` | 完整 OS 仓库，如 `os/arceos`、`os/axvisor`、`os/StarryOS` |
| `rCore` | 少量 rCore 生态组件 |

查看当前配置可使用：

```bash
python3 scripts/repo/repo.py list
python3 scripts/repo/repo.py list --category Hypervisor
```

## 4. `repo.py` 管理命令

[repo.py](/home/zcs/WORKSPACE/tgoskits/scripts/repo/repo.py) 是主仓库里的 subtree 管理入口。它负责：

- 维护 `repos.csv`
- 封装 `git subtree add/pull/push`
- 在未显式指定分支时，按配置或远端默认分支解析目标分支

### 4.1 添加组件

使用 `repo.py add` 命令可以将一个新的组件仓库以 subtree 的方式添加到主仓库中。该命令会自动将组件信息写入 `repos.csv` 配置文件，并执行 `git subtree add` 操作将组件代码拉取到指定的目标目录。

基本用法：

```bash
python3 scripts/repo/repo.py add \
  --url https://github.com/org/new-component \
  --target components/new-component \
  --category Hypervisor
```

指定分支时：

```bash
python3 scripts/repo/repo.py add \
  --url https://github.com/org/new-component \
  --target components/new-component \
  --branch dev \
  --category Hypervisor
```

执行过程：

1. 校验参数
2. 检查 `repos.csv` 是否有重复的 `url` 或 `target_dir`
3. 写入 `repos.csv`
4. 执行 `git subtree add`

### 4.2 移除组件

使用 `repo.py remove` 命令可以从 `repos.csv` 配置文件中移除指定组件的记录。如果需要同时删除组件目录，可以添加 `--remove-dir` 选项。

```bash
python3 scripts/repo/repo.py remove old-component
python3 scripts/repo/repo.py remove old-component --remove-dir
```

### 4.3 切换组件分支

使用 `repo.py branch` 命令可以切换组件所跟踪的分支。该命令会先执行 subtree pull 同步新分支的代码，成功后会更新 `repos.csv` 中对应组件的 `branch` 字段。

```bash
python3 scripts/repo/repo.py branch arm_vcpu dev
python3 scripts/repo/repo.py branch arm_vcpu main
```

### 4.4 批量初始化

使用 `repo.py init` 命令可以根据 `repos.csv` 配置文件批量初始化所有组件的 subtree。这个命令适合在新环境中首次拉取所有组件时使用。

```bash
python3 scripts/repo/repo.py init -f scripts/repo/repos.csv
```

## 5. 手动同步

仓库提供了 `scripts/repo/repo.py` 工具来实现当前主仓库与独立组件仓库的双向同步，以便处理某些特殊情况下的同步。但是，一般情况下，无需手动同步，仓库配置了 CI 来实现自动同步！

### 5.1 从组件仓库同步到主仓库

使用 `repo.py pull` 命令可以将组件仓库的改动同步到主仓库。该命令支持指定组件名称和分支，也可以使用 `--all` 选项批量同步所有组件。如果组件目录尚未加入主仓库，pull 命令会自动执行 add 操作；如果未指定分支，则会优先使用 `repos.csv` 中配置的分支，若该分支为空则自动检测远程默认分支，最后执行 `git subtree pull` 操作。

```bash
python3 scripts/repo/repo.py pull arm_vcpu
python3 scripts/repo/repo.py pull arm_vcpu -b dev
python3 scripts/repo/repo.py pull --all
```

当遇到组件仓库历史被重写、合并冲突难以直接处理或需要重建本地 subtree 等情况时，可以使用 `--force` 选项强制拉取。

```bash
python3 scripts/repo/repo.py pull arm_vcpu --force
```

需要注意的是，当 `repos.csv` 的 `branch` 字段为空，且命令行也没有显式传入 `-b/--branch` 参数时，`repo.py` 会自动检测组件仓库的默认分支。检测逻辑是依次尝试 `main` 分支、`master` 分支、读取 remote 的 `HEAD branch`，最后兜底为 `main`。这套自动检测逻辑主要用于 `repo.py add`、`repo.py pull` 以及 `repo.py list` 中的分支展示。

### 5.2 从主仓库同步到组件仓库

使用 `repo.py push` 命令可以将主仓库中对组件的修改推送到组件仓库。该命令支持指定组件名称、目标分支，也可以使用 `--all` 选项批量推送所有组件。如果未指定目标分支，则会先使用 `repos.csv` 中该组件配置的 `branch`；如果该字段为空，再自动检测远端默认分支。使用 `-f/--force` 选项可以通过带 `+` 的 refspec 强制推送到远端分支。

```bash
python3 scripts/repo/repo.py push arm_vcpu
python3 scripts/repo/repo.py push arm_vcpu -b dev
python3 scripts/repo/repo.py push arm_vcpu -b release/x.y
python3 scripts/repo/repo.py push arm_vcpu -f
python3 scripts/repo/repo.py push --all
```

默认分支解析示例：

- 执行 `python3 scripts/repo/repo.py push ax-cpu -b release`，会直接推送到 `release`
- 如果 `repos.csv` 中 `ax-cpu` 的 `branch` 记录为 `dev`，执行 `python3 scripts/repo/repo.py push ax-cpu` 时会推送到 `dev`
- 如果 `repos.csv` 中 `arm_vcpu` 的 `branch` 为空，执行 `python3 scripts/repo/repo.py push arm_vcpu` 时会自动检测组件仓库默认分支并推送到该分支

`repo.py push` 和 `pull` 在“分支来源优先级”上是相似的，都是优先尊重命令行参数，其次读取 `repos.csv`，最后再自动检测。因此，两者的主要差异不在优先级顺序，而在于“自动检测的对象”和“是否带源分支提示”。

`push` 的优先级如下：

- 显式 `-b/--branch`
- `scripts/repo/repos.csv` 中该组件的 `branch`
- 自动检测组件仓库的远端默认分支

与 `pull` 相比，`push` 有两点额外区别：

- `pull` 的自动检测主要用于决定“从哪个远端分支拉取并合并到主仓库”
- `push` 的自动检测主要用于决定“把主仓库中的 subtree 改动推送到组件仓库的哪个远端分支”

另外，`repo.py push` 会额外检测当前主仓库源分支并打印提示，例如 `tgoskits main -> ax-cpu release`。这个源分支信息仅用于日志展示，不参与手工 `push` 命令本身的目标分支解析。

需要注意的是，`git subtree push` 本身并不支持单独的 `--force` 参数，强制推送是通过 refspec 形式（例如 `+dev`）实现的。如果组件仓库远端已经前进，通常应先做同步确认，再决定是否使用强制推送。

## 6. 自动同步方案

当前仓库采用两条自动同步链路实现主仓库与组件仓库之间的双向同步。第一条链路是主仓库 `main` 或 `dev` 分支收到修改后，按规则把改动推到组件仓库的对应分支；第二条链路是组件仓库 `main` 或 `master` 收到修改后，自动向主仓库 `main` 发起 subtree 同步 PR。这两条链路共同构成了完整的双向同步机制，确保主仓库和组件仓库之间的代码能够及时、准确地同步。

### 6.1 从主仓库到组件仓库

主仓库使用 [push.yml](/home/zcs/WORKSPACE/tgoskits/.github/workflows/push.yml) 实现向组件仓库的自动推送。该工作流会在主仓库 `main` 或 `dev` 分支收到 push，或手动触发时，自动检测本次提交中修改的文件，识别受影响的组件目录，并将对应的 subtree 改动按分支映射规则推送到各组件仓库。

#### 6.1.1 触发方式

工作流支持两种触发方式：当有代码推送到主仓库的 `main` 或 `dev` 分支时会自动触发；也可以通过 GitHub Actions 的 `workflow_dispatch` 手动触发执行。

- `push` 到主仓库 `main`
- `push` 到主仓库 `dev`
- 手动触发 `workflow_dispatch`

#### 6.1.2 工作流行为

工作流执行时会先 checkout 主仓库的完整历史，然后根据本次 push 修改的文件范围与 `repos.csv` 中配置的组件路径进行匹配，识别出受影响的组件。对于每个受影响的组件，工作流会先根据当前主仓库分支和组件仓库所属组织计算目标分支，然后把该目标分支通过 `-b` 显式传给 `repo.py push`。`repo.py` 在这里负责执行 subtree push，并自动检测当前源分支用于日志提示，本身不承载 CI 的映射规则。

工作流会：

1. checkout 主仓库完整历史
2. 根据 `github.event.before..github.sha` 计算本次 push 修改过的文件
3. 从 `repos.csv` 提取所有 `target_dir`
4. 找出受影响的组件目录
5. 使用 `SUBTREE_PUSH_TOKEN` 配置认证
6. 对每个变更组件先计算目标分支，再执行 `python3 scripts/repo/repo.py push <repo_name> -b <target_branch>`

默认按以下规则自动映射，但手动触发时仍可用 `branch` 输入统一覆盖：

- 主仓库 `main` -> `arceos-org/*` 的 `release`
- 主仓库 `main` -> 非 `arceos-org/*` 的 `main`
- 主仓库 `dev` -> 所有组件仓库的 `dev`

需要特别注意的是，主仓库 CI 的分支映射规则并不是由 `repo.py push` 内部决定的，而是由 [push.yml](/home/zcs/WORKSPACE/tgoskits/.github/workflows/push.yml) 先计算出目标分支，再显式通过 `-b` 传给 `repo.py push`。`repo.py` 在本地优先读取当前 Git 分支，在 GitHub Actions 这类 detached HEAD 场景下则会回退到环境变量。

#### 6.1.3 认证

主仓库需要配置一个名为 `SUBTREE_PUSH_TOKEN` 的 Secret 来完成跨仓库推送认证。由于存在跨组织组件仓库，无法依赖主仓库默认的 `GITHUB_TOKEN` 完成跨仓库推送，因此必须使用具有跨仓库访问权限的 Personal Access Token。

配置要求：
- Secret 名称：`SUBTREE_PUSH_TOKEN`
- 类型：Classic Personal Access Token
- 权限：至少包含 `repo`

#### 6.1.4 推送策略

当前默认策略分两类：当改动来自主仓库 `dev` 分支时，统一推送到组件仓库的 `dev` 分支，作为集成分支；当改动来自主仓库 `main` 分支时，`arceos-org/*` 组件推送到 `release`，非 `arceos-org/*` 组件推送到 `main`。这样既保留了 `dev` 作为日常集成缓冲层，也让主仓库稳定分支可以对不同组织的组件仓库采用更合适的落点分支。

#### 6.1.5 完整流程示例

下面的示例展示“开发者先在主仓库 `main` 修改组件，再由主仓库自动按规则推到独立组件仓库目标分支”的完整处理流程。

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│            场景 A：主仓库修改组件 -> 自动推送到组件仓库映射分支            │
└─────────────────────────────────────────────────────────────────────────────┘

时间线    主仓库 (tgoskits)              GitHub Actions                 组件仓库

T1        开发者修改组件代码
          components/axcpu/src/...
          git commit && git push origin main
          │
          ├──────────────────────────────► T2 触发 `.github/workflows/push.yml`
          │                                   事件：`push(main)` 或手动触发
          │
          │                               T3 检测本次变更文件
          │                                  `git diff before..sha`
          │                                  匹配 `repos.csv` 中的 `target_dir`
          │
          │                               T4 配置跨仓库认证
          │                                  使用 `SUBTREE_PUSH_TOKEN`
          │                                  配置 git credential helper
          │
          │                               T5 对每个受影响组件计算目标分支
          │                                  `main + arceos-org/* -> release`
          │                                  `main + others -> main`
          │                                  `dev -> all dev`
          │
          │                               T6 执行 push
          │                                  `python3 scripts/repo/repo.py push`
          │                                  `  <repo_name> -b <target_branch>`
          │                                  底层等价于 `git subtree push`
          │
          └───────────────────────────────────────────────────────────────► T7 推送到映射后的目标分支
                                                                           `arceos-org/* -> release`
                                                                           `others -> main`
                                                                           或手动覆盖分支

                                                                           T8 组件仓库维护者继续测试、
                                                                              整理并合并到 `main/master`
```

### 6.2 从组件仓库到主仓库

组件仓库使用模板 [scripts/push.yml](/home/zcs/WORKSPACE/tgoskits/scripts/push.yml) 实现向主仓库的自动同步。将该模板复制到组件仓库的 `.github/workflows/` 目录下后，当组件仓库的 `main` 或 `master` 分支收到新的提交时，工作流会自动 checkout 主仓库，执行精确到当前提交 SHA 的 subtree pull 操作，然后向主仓库创建或更新同步 PR。之所以监听 `main/master` 而不是 `dev` 分支，是为了兼容不同组件仓库的默认主分支命名，同时避免主仓库推到组件 `dev` 后形成自动循环。

#### 6.2.1 触发方式

工作流会在以下情况下触发：当有代码推送到组件仓库的 `main` 或 `master` 分支时会自动触发；也可以通过 GitHub Actions 的 `workflow_dispatch` 手动触发执行。

- `push` 到组件仓库 `main`
- `push` 到组件仓库 `master`
- 手动触发 `workflow_dispatch`

#### 6.2.2 工作流行为

组件仓库中的 workflow 执行时会使用配置的认证令牌 checkout 主仓库的 `main` 分支，从主仓库的 `repos.csv` 中根据当前组件仓库 URL 查找对应的 `target_dir`，然后在主仓库中创建或重置同步分支。接下来执行精确到当前提交 SHA 的 subtree pull 操作，如果有实际变更则将同步分支推到主仓库并创建或更新指向 `main` 的 PR。

1. 使用 `PARENT_REPO_TOKEN` checkout 主仓库 `main`
2. 从主仓库的 `scripts/repo/repos.csv` 中按当前组件仓库 URL 查找 `target_dir`
3. 在主仓库中创建或重置同步分支，例如 `subtree-sync/<repo>-main`
4. 执行：`git subtree pull --prefix=<target_dir> <component_repo_url> <commit_sha>`
5. 如果没有实际变更，则跳过
6. 如果有变更，则将同步分支推到主仓库
7. 创建或更新一个指向主仓库 `main` 的 PR

#### 6.2.3 认证

组件仓库需要配置一个名为 `PARENT_REPO_TOKEN` 的 Secret 来完成对主仓库的访问。该 token 需要能够 checkout 主仓库、向主仓库推送同步分支以及在主仓库创建 PR，因此必须具有完整的仓库访问权限。

配置要求：
- Secret 名称：`PARENT_REPO_TOKEN`
- 类型：Classic Personal Access Token
- 权限：至少包含 `repo`

#### 6.2.4 完整流程示例

下面的示例展示“组件仓库 `main/master` 收到新提交后，自动向主仓库发起 subtree 同步 PR”的完整处理流程。

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│         场景 B：组件仓库 main/master 更新 -> 自动向主仓库创建同步 PR        │
└─────────────────────────────────────────────────────────────────────────────┘

时间线    组件仓库                         GitHub Actions                  主仓库
  │
  │     ┌──────────────────────────────────────┐
  T1    │ 组件仓库 main/master 收到新提交      │
  │     │ 可能来自独立开发                     │
  │     │ 也可能来自 dev 分支整理后合并        │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │     ┌──────────────────────────────────────┐
  T2    │ 触发组件仓库中的 .github/workflows/  │
  │     │ push.yml（由 scripts/push.yml 复制） │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │     ┌──────────────────────────────────────┐
  T3    │ checkout 主仓库 main                 │
  │     │ 使用 PARENT_REPO_TOKEN               │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │     ┌──────────────────────────────────────┐
  T4    │ 在主仓库 repos.csv 中定位当前组件     │
  │     │ 按组件仓库 URL 查找 target_dir        │
  │     │ 例如：components/axcpu               │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │     ┌──────────────────────────────────────┐
  T5    │ 创建或重置同步分支                    │
  │     │ subtree-sync/<repo>-main             │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │     ┌──────────────────────────────────────┐
  T6    │ 执行 subtree pull                    │
  │     │ git subtree pull                     │
  │     │   --prefix=<target_dir>              │
  │     │   <component_repo_url> <commit_sha>  │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │     ┌──────────────────────────────────────┐
  T7    │ 判断是否产生新提交                    │
  │     │ 无变化：直接结束                      │
  │     │ 有变化：push sync branch 到主仓库     │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │                                             ┌───────────────────────────┐
  T8                                          ─►│ 在主仓库创建或更新 PR     │
  │                                             │ base: main                │
  │                                             │ head: subtree-sync/...    │
  │                                             └────────────┬──────────────┘
  │                                                          │
  │                                                          ▼
  │                                             ┌───────────────────────────┐
  T9                                          ─►│ 主仓库评审、测试、合并 PR │
  │                                             │ subtree 更新进入 main     │
  │                                             └───────────────────────────┘
  ▼
```

## 7. 版本发布

TGOSKits 当前使用 [`.github/workflows/release.yml`](/home/zcs/WORKSPACE/tgoskits/.github/workflows/release.yml) 和 [`release-plz.toml`](/home/zcs/WORKSPACE/tgoskits/release-plz.toml) 管理版本发布。它不再依赖“维护者先手工打 tag，再触发发布”的模式，而是改为“先自动生成版本 PR，合并后再正式发布”。

### 7.1 目标

这套发布流程主要解决三个问题：

- 自动识别 workspace 中哪些 crate 发生了会影响发布的改动
- 自动计算版本号并生成版本变更 PR，而不是手工逐个改 `Cargo.toml`
- 在版本 PR 合并后，自动创建 tag、GitHub Release，并发布到 crates.io

### 7.2 触发时机

`release.yml` 会在以下场景触发：

- push 到 `main`
- push 到 `dev`
- 手工执行 `workflow_dispatch`

也就是说，`main` 和 `dev` 都会参与“版本编排”，并且两条分支都会各自生成 release PR、各自执行正式发布。它们在流程上是两条独立发布线。

### 7.3 发布流程

当前 workflow 里有两个固定 job：

- `Prepare release PR`：负责准备版本改动，不直接发布
- `Publish release`：负责基于已经合入分支的版本提交执行正式发布

两者的分工可以先概括成一句话：

- `Prepare release PR` 决定“应该发布什么版本”
- `Publish release` 负责“把这个版本真正发出去”

这套行为依赖 [`release-plz.toml`](/home/zcs/WORKSPACE/tgoskits/release-plz.toml) 中的配置：

```toml
[workspace]
release_always = false
```

这个配置的含义是：普通 push 不会直接发版。只有当版本改动已经通过 `release-plz-*` 机器人分支对应的 release PR 合入目标分支后，`Publish release` 才会真正执行发布。

下面按三种最常见的场景来理解整个流程。

#### 7.3.1 `dev` 收到 push

当新的功能提交进入 `dev` 时，会触发一次 `release.yml`：

1. `Prepare release PR` 运行
2. 它会分析从上一个已发布版本以来，哪些 crate 发生了变化
3. 然后生成版本号、内部依赖版本、`Cargo.lock`、`CHANGELOG.md` 等改动
4. 这些改动不会直接提交到 `dev`，而是先写到机器人分支 `release-plz-dev`
5. 接着 CI 创建或更新 PR：`release-plz-dev -> dev`
6. 同一次 workflow 中，`Publish release` 也会被调度
7. 但因为此时 `dev` 上还没有合入这次 release PR，所以不会真正发布新版本

这意味着：

- 第一次 push 到 `dev`，看到的结果通常是“自动生成了一个 release PR”
- 此时仓库里只是有了“待发布的版本改动”，还没有真正发布

#### 7.3.2 `main` 收到 push

当新的提交进入 `main` 时，流程和 `dev` 基本相同，只是目标分支换成了 `main`：

1. `Prepare release PR` 运行
2. 生成面向 `main` 的版本改动
3. 推送机器人分支 `release-plz-main`
4. 创建或更新 PR：`release-plz-main -> main`
5. `Publish release` 同样会被调度
6. 如果这次 push 本身不是 release PR 合并后的结果，那么它也不会真正发版

因此，对 `main` 来说，普通开发合并或同步提交进入主线后，首先发生的也是“生成一个可审阅的 release PR”，而不是立即创建 tag 或立刻发布 crates.io。

#### 7.3.3 `dev` 向 `main` 合并

当维护者把 `dev` 合并到 `main` 时，`main` 会收到一次新的 push。这个场景要分两步看：

第一步：

- `main` 因为合并 `dev` 而触发 workflow
- `Prepare release PR` 重新按 `main` 当前状态计算版本
- 如果 `main` 上还没有对应的 release PR，就会创建或更新 `release-plz-main -> main`

第二步：

- 维护者审阅并合并 `release-plz-main -> main`
- `main` 因为这次 release PR 合并再次触发 workflow
- 这一次 `Publish release` 才会读取刚刚合入主线的版本提交
- 然后创建 git tag、创建 GitHub Release，并把可发布 crate 发到 crates.io

所以，“`dev` 合到 `main`”本身并不等于已经发布稳定版本。真正的稳定版发布，发生在 `main` 上的 release PR 被合并之后。

从分支分工上看，这里也可以顺便这样理解：

- `dev` 更偏向集成与预览发布线，适合先汇聚功能改动，再生成并合并面向 `dev` 的 release PR
- `main` 更偏向稳定发布线，通常是在 `dev` 经过一轮集成后，再把结果合到 `main`，生成并合并面向 `main` 的 release PR

因此，推荐的日常节奏是：

- 开发 PR 一律先进入 `dev`
- 当需要发布集成版本时，直接合并 `dev` 上的 release PR
- 待一轮集成稳定后，再把 `dev` 合入 `main`
- 当需要发布稳定版本时，再合并 `main` 上的 release PR

#### 7.3.4 最终版本是如何发布出去的

可以把完整发布动作理解成下面这个闭环：

```text
代码提交进入分支
  -> Prepare release PR 生成版本改动
  -> 维护者审阅并合并 release PR
  -> 分支再次触发 workflow
  -> Publish release 创建 tag / GitHub Release / crates.io 发布
```

也就是说，最终版本发布一定经过这三个阶段：

1. 发现改动：识别哪些 crate 需要发版
2. 固化版本：把版本号和相关文件改动合入 `main` 或 `dev`
3. 正式发布：基于已经合入的版本提交执行 tag、GitHub Release 和 `cargo publish`

#### 7.3.5 版本号在什么时候更新

版本号不是在 `Publish release` 阶段才修改的，而是在 `Prepare release PR` 阶段就已经生成好了。release PR 通常会包含：

- 各 crate 的 `Cargo.toml` 版本号
- 依赖了这些 crate 的内部依赖版本
- `Cargo.lock`
- `CHANGELOG.md`（如果对应 crate 启用了 changelog 生成）

对 TGOSKits 这种 workspace 来说，版本号更新分两类：

- 对于在各自 `Cargo.toml` 中显式写了 `version = "..."` 的 crate，release-plz 会直接修改该 crate 自己的版本号
- 对于使用 `version.workspace = true` 的 crate，实际版本源头在根 [`Cargo.toml`](/home/zcs/WORKSPACE/tgoskits/Cargo.toml) 的 `[workspace.package].version`，因此 release PR 也可能直接修改根 `Cargo.toml`

因此：

- release PR 合并前，版本改动只存在于 release PR 和机器人分支中
- release PR 合并后，版本改动才真正进入 `main` 或 `dev`
- `Publish release` 发布的是“已经合入分支的版本”，而不是临时计算出来但尚未合并的版本

### 7.4 Tag 与 Release 命名

当 `main` 或 `dev` 分支上的 release PR 合并后，`release-plz` 会在当前仓库创建 git tag，并同步创建 GitHub Release。

在没有额外自定义 tag 模板时，多 crate workspace 默认采用：

```text
<package_name>-v<version>
```

例如：

- `aarch64_sysreg-v0.1.2`
- `axplat-dyn-v0.3.1`

如果未来希望统一使用整仓 tag（例如 `v0.3.0`），则需要进一步调整 `release-plz` 配置，而不是依赖默认行为。

### 7.5 需要的 Secrets 与权限

要让发布流程完整生效，仓库通常需要准备以下 secrets：

- `GITHUB_TOKEN`：GitHub Actions 默认提供，用于创建或更新 release PR、创建 tag 和 GitHub Release
- `RELEASE_PLZ_TOKEN`：可选；如果默认 `GITHUB_TOKEN` 在仓库策略下权限不足，可改用单独 token
- `CARGO_REGISTRY_TOKEN`：必需；用于向 crates.io 发布 crate

如果缺少 `CARGO_REGISTRY_TOKEN`，release PR 仍然可以创建，但 `main` 和 `dev` 上的正式 `cargo publish` 都会失败。

### 7.6 维护建议

为了让发布流程更稳定，建议遵循以下约定：

- 不要手工提前修改一批 crate 版本号，优先让 release-plz 统一维护
- 不要手工创建“准备发布”的 tag，避免和自动生成的 tag 冲突
- 合并 release PR 前，先确认 CI、changelog 和待发布 crate 列表都符合预期
- 若 `main` 和 `dev` 都发布到同一个 cargo registry，需要额外留意两条分支的版本号不要互相冲突
- 若某个 crate 不应发布到 crates.io，应在其 `Cargo.toml` 中明确配置 `publish = false`
- 若需要统一调整 tag 命名、changelog 策略或分支行为，应优先修改 `release-plz.toml`

## 8. 开发场景示例

### 8.1 当前 CI 拓扑

当前的双向自动同步机制由两条独立的 CI 链路组成：主仓库的 `push.yml` 负责将主仓库中的组件改动按规则推送到组件仓库的映射分支；组件仓库的 `push.yml`（由主仓库的 `scripts/push.yml` 复制而来）负责在组件仓库主线更新时向主仓库发起同步 PR。这两条链路共同确保了主仓库和各组件仓库之间的代码能够及时、准确地双向同步。

```text
主仓库 main / dev 分支
  └─ .github/workflows/push.yml
       └─ git subtree push
            └─ 按规则推到组件仓库 release / main / dev

组件仓库 main/master
  └─ .github/workflows/push.yml  (由 scripts/push.yml 复制而来)
       └─ checkout 主仓库
       └─ git subtree pull --prefix=<target_dir> <repo> <sha>
       └─ push sync branch
       └─ create PR to 主仓库 main
```

这两条链路合起来，构成当前实际生效的双向同步机制。

### 8.2 端到端闭环示意

下面这张图把“主仓库改组件 -> 推到组件仓库映射分支 -> 组件仓库合并到 main -> 反向给主仓库提 PR -> 主仓库合并”的完整闭环串起来。

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                           双向同步完整闭环                                  │
└─────────────────────────────────────────────────────────────────────────────┘

主仓库 tgoskits                                               组件仓库

功能分支
  │
  ├─ PR ───────────────────────────────────────────────► dev
  │                                                      │
  │                                                      ├─ `push.yml`
  │                                                      └─ subtree push 到组件仓库 `dev`
  │
  └──────────────────────────────────────────────────────► main
                                                         │
                                                         ├─ `push.yml`
                                                         └─ subtree push
                                                            `arceos-org/* -> release`
                                                            `others -> main`

组件仓库接收主仓库推送后的流转：

release / main / dev
  │
  ├─ 维护者测试、整理
  └─ 合并到组件仓库 `main/master`
          │
          ├─ 组件仓库 `push.yml`
          ├─ checkout 主仓库
          ├─ `git subtree pull --prefix=<target_dir> <repo> <sha>`
          └─ 创建 / 更新 PR 到主仓库 `main`
                                   │
                                   ▼
                            subtree-sync/<repo>-main
                                   │
                                   ▼
                              主仓库 `main`
                              评审 / 测试 / 合并
```

### 8.3 典型场景

#### 8.3.1 在主仓库里改了组件代码

当你在主仓库的 `components/` 目录下修改了某个组件的代码并提交推送到 `main` 或 `dev` 分支后，主仓库的 `push.yml` 工作流会自动识别受影响的组件，并按规则将对应的 subtree 改动推送到组件仓库的映射分支。整个流程完全自动化，开发者无需手动执行任何同步操作。

典型流程：

1. 在 `components/<name>/` 修改代码
2. 提交并推送到主仓库 `main` 或 `dev`
3. 主仓库 `push.yml` 自动识别受影响组件
4. 自动把对应 subtree 推到组件仓库 `release` / `main` / `dev`

#### 8.3.2 在组件仓库里改了主线代码

当组件仓库的 `main` 或 `master` 分支合入新的提交后，组件仓库的 workflow 会自动 checkout 主仓库并执行精确到当前 SHA 的 `git subtree pull` 操作，然后自动向主仓库创建或更新同步 PR。主仓库维护者只需要评审、测试并合并该 PR 即可完成同步。

典型流程：

1. 在组件仓库 `main` 或 `master` 合入新提交
2. 组件仓库 workflow 自动 checkout 主仓库
3. 自动执行一次精确到当前 SHA 的 `git subtree pull`
4. 自动向主仓库创建或更新同步 PR

#### 8.3.3 组件仓库远端已前进，主仓库要强制回推

当组件仓库的远端分支已经前进，而主仓库需要强制回推改动时，可以手动使用 `python3 scripts/repo/repo.py push <repo_name> -f` 命令。但强制推送意味着会重写组件仓库目标分支的历史，建议只在明确知道该分支是主仓库控制的集成分支、已经确认组件仓库远端改动不需要保留、且团队已经对这次历史覆盖达成一致的情况下使用。
