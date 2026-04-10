# TGOSKits 仓库管理指南

本文档介绍 TGOSKits 主仓库如何使用 Git Subtree 管理独立组件仓库，以及当前仍然生效的仓库组织方式。

## 1. 概述

TGOSKits 是一个统一工作区仓库。它通过 Git Subtree 将多个独立组件仓库整合到主仓库中，既保留组件的独立开发能力，又支持在主仓库中做跨系统联调、统一测试和集中维护。

当前仓库仍然保留 subtree 管理脚本与手动 `pull` / `push` 能力，但此前文档中描述的“双向自动同步”CI 流程已经移除，不再是当前仓库的一部分。因此：

- 主仓库不会再通过 GitHub Actions 自动把组件改动推送到独立组件仓库
- 组件仓库也不会再自动向主仓库创建 subtree 同步 PR
- 如需同步，维护者需要显式执行 `scripts/repo/repo.py` 提供的手动命令

### 1.1 核心特性

- 统一工作区：在一个仓库里同时开发组件、OS 和平台相关代码
- 历史保留：Subtree 保留组件的提交历史，便于追溯问题来源
- 配置显式：所有组件来源、路径和分支信息集中记录在 `scripts/repo/repos.csv`
- 手动可控：组件同步由维护者显式执行，避免隐藏的跨仓库自动写入

### 1.2 仓库结构

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
│   └── test.yml               # 当前保留的主 CI
└── docs/
```

需要特别注意：

- `components/` 并不是按 `Hypervisor/ArceOS/Starry` 建目录分层
- 大多数组件直接平铺在 `components/` 下
- 组件分类主要来自 `scripts/repo/repos.csv` 中的 `category` 字段，而不是目录层级

## 2. 分支管理

TGOSKits 主仓库采用三层分支策略：`main` 作为稳定发布分支，`dev` 作为集成分支，开发者在个人功能分支上完成修改后再通过 PR 合入 `dev`。

### 2.1 分支总览

```text
功能分支 (feature/*, fix/*, ...)
    │  开发者本地开发、自测
    │
    │  PR（禁止直接发到 main）
    ▼
  dev  集成分支
    │  汇聚开发功能、执行 CI
    │
    │  定期合并到 main
    ▼
 main  稳定发布分支
    │  作为稳定基线维护
    └──────────────────────────
```

### 2.2 `main` 分支

`main` 分支是仓库的稳定基线，适合承接已经在 `dev` 上完成集成验证的内容。

核心规则：

- 禁止直接 push，变更应通过受控流程进入
- 定期从 `dev` 合并已验证的改动
- 作为对外展示和稳定使用的主线

### 2.3 `dev` 分支

`dev` 分支是日常开发的主战场，所有功能开发和 bug 修复最终都汇聚到这里。

核心规则：

- 所有常规开发 PR 默认进入 `dev`
- `dev` 上的提交需要保持可编译、可测试
- `main` 定期从 `dev` 合并

### 2.4 功能分支

开发者基于 `dev` 分支创建功能分支进行开发，命名建议如下：

| 类型 | 命名格式 | 示例 |
|------|----------|------|
| 新功能 | `feature/<描述>` | `feature/vm-pause-resume` |
| Bug 修复 | `fix/<描述>` | `fix/pl011-uart-overflow` |
| 重构 | `refactor/<描述>` | `refactor/axvm-crate-split` |
| 文档 | `docs/<描述>` | `docs/repo-guide` |
| 实验性 | `experiment/<描述>` | `experiment/riscv-smp` |

### 2.5 PR 规则

所有代码变更应先进入 `dev`，再由维护者视情况合并到 `main`。

```text
1. 开发者从 dev 创建功能分支
2. 在功能分支完成开发和自测
3. 向 dev 提交 PR
4. CI 与代码评审通过后合并
5. 维护者按节奏将 dev 合并到 main
```

建议遵循以下约定：

| 规则 | 说明 |
|------|------|
| **禁止直接 PR 到 `main`** | 日常开发 PR 的目标分支应为 `dev` |
| **功能分支基于 `dev`** | 避免从 `main` 拉出长期开发分支造成额外冲突 |
| **保持分支更新** | 开发周期较长时，应及时同步 `dev` |
| **PR 描述完整** | 写清变更说明、测试方法和关联 issue |

## 3. 组件配置

### 3.1 `repos.csv`

Git Subtree 不像 Git Submodule 那样自带 `.gitmodules`。因此，TGOSKits 使用 [`scripts/repo/repos.csv`](/home/zcs/WORKSPACE/tgoskits/scripts/repo/repos.csv) 作为组件来源配置清单。

它记录了：

- 组件仓库 URL
- 建议跟踪的分支
- 组件在主仓库中的目标目录
- 组件分类与备注

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

### 3.3 查看组件清单

查看当前配置可使用：

```bash
python3 scripts/repo/repo.py list
python3 scripts/repo/repo.py list --category Hypervisor
```

## 4. `repo.py` 管理命令

[`scripts/repo/repo.py`](/home/zcs/WORKSPACE/tgoskits/scripts/repo/repo.py) 是主仓库里的 subtree 管理入口。它负责：

- 维护 `repos.csv`
- 封装 `git subtree add/pull/push`
- 在未显式指定分支时，按配置或远端默认分支解析目标分支

### 4.1 添加组件

使用 `repo.py add` 可以将新的组件仓库加入主仓库：

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

该命令会：

1. 校验参数
2. 检查 `repos.csv` 是否有重复的 `url` 或 `target_dir`
3. 写入 `repos.csv`
4. 执行 `git subtree add`

### 4.2 移除组件

```bash
python3 scripts/repo/repo.py remove old-component
python3 scripts/repo/repo.py remove old-component --remove-dir
```

`remove` 会先从 `repos.csv` 删除记录；加上 `--remove-dir` 时还会删除工作区中的组件目录。

### 4.3 切换组件跟踪分支

```bash
python3 scripts/repo/repo.py branch arm_vcpu dev
python3 scripts/repo/repo.py branch arm_vcpu main
```

`branch` 会先执行一次 subtree pull 同步目标分支内容，成功后再更新 `repos.csv` 中对应组件的 `branch` 字段。

### 4.4 批量初始化

```bash
python3 scripts/repo/repo.py init -f scripts/repo/repos.csv
```

`init` 适合在新环境中按 `repos.csv` 批量初始化所有 subtree。

### 4.5 手动从组件仓库拉取

使用 `pull` 可以将组件仓库中的改动同步到主仓库：

```bash
python3 scripts/repo/repo.py pull arm_vcpu
python3 scripts/repo/repo.py pull arm_vcpu -b dev
python3 scripts/repo/repo.py pull --all
```

如果组件目录尚未加入主仓库，`pull` 会先执行 add；如果未指定分支，则优先读取 `repos.csv` 中的 `branch`，否则自动检测组件仓库默认分支。

当遇到组件仓库历史被重写、冲突难以直接处理或需要重建 subtree 时，可以使用 `--force`：

```bash
python3 scripts/repo/repo.py pull arm_vcpu --force
```

### 4.6 手动向组件仓库推送

使用 `push` 可以把主仓库中的组件改动显式推送到组件仓库：

```bash
python3 scripts/repo/repo.py push arm_vcpu
python3 scripts/repo/repo.py push arm_vcpu -b dev
python3 scripts/repo/repo.py push arm_vcpu -b release/x.y
python3 scripts/repo/repo.py push arm_vcpu -f
python3 scripts/repo/repo.py push --all
```

分支解析优先级如下：

1. 显式 `-b/--branch`
2. `scripts/repo/repos.csv` 中该组件的 `branch`
3. 自动检测组件仓库默认分支

如果调用时没有解析出目标分支，`repo.py push` 会退回默认 `dev` 分支。

`-f/--force` 会通过强制推送覆盖远端目标分支历史，只应在确认该目标分支由主仓库统一维护、且团队已经确认允许覆盖时使用。

## 5. 当前同步方式

当前仓库不再提供双向自动同步。与组件仓库之间的代码流转，需要由维护者主动执行 `pull` 或 `push`。

推荐把同步理解成两个明确场景：

- 组件仓库的改动要并回 TGOSKits：在主仓库执行 `repo.py pull`
- TGOSKits 中的组件改动要回推到独立仓库：在主仓库执行 `repo.py push`

这意味着同步动作不再隐式发生在 CI 中，而是成为显式维护操作。这样做的直接结果是：

- 跨仓库写入不会在普通 push 后自动发生
- 同步时机和目标分支由维护者明确决定
- 文档、代码评审和发布流程可以与同步动作解耦

## 6. 常见场景

### 6.1 在主仓库中修改组件后，回推到组件仓库

典型流程：

1. 在 `components/<name>/` 修改并验证代码
2. 将改动提交到当前工作分支
3. 由维护者显式执行 `python3 scripts/repo/repo.py push <repo_name> -b <branch>`
4. 到组件仓库确认结果并继续后续流程

如果目标分支不是 `repos.csv` 中登记的分支，记得显式传入 `-b/--branch`。

### 6.2 组件仓库已有新提交，需要同步回主仓库

典型流程：

1. 确认要同步的组件仓库和目标分支
2. 在主仓库执行 `python3 scripts/repo/repo.py pull <repo_name> -b <branch>`
3. 处理可能出现的合并冲突
4. 在主仓库完成测试并提交同步结果

### 6.3 新环境首次初始化组件

```bash
python3 scripts/repo/repo.py init -f scripts/repo/repos.csv
```

如果只想初始化少量组件，也可以使用 `add` 单独加入。

### 6.4 查看仓库当前是否仍有自动同步

当前主仓库默认只保留常规测试 workflow。历史上的 `push.yml` / `release.yml` 自动化流程已从启用状态中移除，不应再按“push 后自动同步”或“自动创建同步 PR”的方式理解本仓库。
