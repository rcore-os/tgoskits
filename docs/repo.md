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

## 2. 组件配置

### 2.1 为什么需要 `repos.csv`

Git Subtree 不像 Git Submodule 那样自带 `.gitmodules`。这意味着：

- Git 本身不会持久记录“某个目录对应哪个远程仓库”
- 临时 remote 在命令执行后会被清理
- 单靠 Git 命令无法完整恢复组件来源信息

因此，TGOSKits 使用 [repos.csv](/home/zcs/WORKSPACE/tgoskits/scripts/repo/repos.csv) 作为组件来源配置清单。

### 2.2 字段说明

`repos.csv` 的格式为：

```text
url,branch,target_dir,category,description
```

字段含义如下：

| 字段 | 必填 | 说明 | 示例 |
|------|:----:|------|------|
| `url` | 是 | 组件仓库 URL | `https://github.com/arceos-org/axcpu` |
| `branch` | 否 | 建议跟踪的分支；留空时由 `repo.py` 自动检测 | `dev` |
| `target_dir` | 是 | 组件在主仓库中的路径 | `components/axcpu` |
| `category` | 否 | 组件分类 | `ArceOS` |
| `description` | 否 | 备注描述 | `CPU abstraction component` |

### 2.3 当前组件分布

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

## 3. `repo.py` 管理命令

[repo.py](/home/zcs/WORKSPACE/tgoskits/scripts/repo/repo.py) 是主仓库里的 subtree 管理入口。它负责：

- 维护 `repos.csv`
- 封装 `git subtree add/pull/push`
- 在未显式指定分支时，按规则确定目标分支

### 3.1 添加组件

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

### 3.2 移除组件

使用 `repo.py remove` 命令可以从 `repos.csv` 配置文件中移除指定组件的记录。如果需要同时删除组件目录，可以添加 `--remove-dir` 选项。

```bash
python3 scripts/repo/repo.py remove old-component
python3 scripts/repo/repo.py remove old-component --remove-dir
```

### 3.3 切换组件分支

使用 `repo.py branch` 命令可以切换组件所跟踪的分支。该命令会先执行 subtree pull 同步新分支的代码，成功后会更新 `repos.csv` 中对应组件的 `branch` 字段。

```bash
python3 scripts/repo/repo.py branch arm_vcpu dev
python3 scripts/repo/repo.py branch arm_vcpu main
```

### 3.4 批量初始化

使用 `repo.py init` 命令可以根据 `repos.csv` 配置文件批量初始化所有组件的 subtree。这个命令适合在新环境中首次拉取所有组件时使用。

```bash
python3 scripts/repo/repo.py init -f scripts/repo/repos.csv
```

## 4. 分支解析规则

### 4.1 `add` / `pull` 的默认分支

当 `repos.csv` 的 `branch` 字段为空，且命令行也没有显式传入 `-b/--branch` 参数时，`repo.py` 会自动检测组件仓库的默认分支。检测逻辑是依次尝试 `main` 分支、`master` 分支、读取 remote 的 `HEAD branch`，最后兜底为 `main`。这套自动检测逻辑主要用于 `repo.py add`、`repo.py pull` 以及 `repo.py list` 中的分支展示。

### 4.2 `push` 的默认分支

`repo.py push` 的默认分支行为与 `pull` 不同。如果显式传入 `-b/--branch` 参数，则会推送到指定的分支；如果未传入分支参数，则默认推送到组件仓库的 `dev` 分支。例如，执行 `python3 scripts/repo/repo.py push axcpu` 等价于执行 `python3 scripts/repo/repo.py push axcpu -b dev`。这样的设计是为了让主仓库向组件仓库同步时，默认走组件仓库的集成分支，而不是直接改写对方的 `main` 分支。

## 5. 手动同步

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

### 5.2 从主仓库同步到组件仓库

使用 `repo.py push` 命令可以将主仓库中对组件的修改推送到组件仓库。该命令支持指定组件名称、目标分支，也可以使用 `--all` 选项批量推送所有组件。如果未指定分支，则默认推送到组件仓库的 `dev` 分支。使用 `-f/--force` 选项可以通过带 `+` 的 refspec 强制推送到远端分支。

```bash
python3 scripts/repo/repo.py push arm_vcpu
python3 scripts/repo/repo.py push arm_vcpu -b dev
python3 scripts/repo/repo.py push arm_vcpu -b release/x.y
python3 scripts/repo/repo.py push arm_vcpu -f
python3 scripts/repo/repo.py push --all
```

需要注意的是，`git subtree push` 本身并不支持单独的 `--force` 参数，强制推送是通过 refspec 形式（例如 `+dev`）实现的。如果组件仓库远端已经前进，通常应先做同步确认，再决定是否使用强制推送。

## 6. 自动同步方案

当前仓库采用两条自动同步链路实现主仓库与组件仓库之间的双向同步。第一条链路是主仓库 `dev` 分支收到修改后，自动把改动推到组件仓库的 `dev` 分支；第二条链路是组件仓库 `main` 或 `master` 收到修改后，自动向主仓库 `main` 发起 subtree 同步 PR。这两条链路共同构成了完整的双向同步机制，确保主仓库和组件仓库之间的代码能够及时、准确地同步。

### 6.1 从主仓库到组件仓库

主仓库使用 [push.yml](/home/zcs/WORKSPACE/tgoskits/.github/workflows/push.yml) 实现向组件仓库的自动推送。该工作流会在主仓库 `dev` 分支收到 push 或手动触发时，自动检测本次提交中修改的文件，识别受影响的组件目录，并将对应的 subtree 改动推送到各组件仓库的 `dev` 分支。

#### 6.1.1 触发方式

工作流支持两种触发方式：当有代码推送到主仓库的 `dev` 分支时会自动触发；也可以通过 GitHub Actions 的 `workflow_dispatch` 手动触发执行。

- `push` 到主仓库 `dev`
- 手动触发 `workflow_dispatch`

#### 6.1.2 工作流行为

工作流执行时会先 checkout 主仓库的完整历史，然后根据本次 push 修改的文件范围与 `repos.csv` 中配置的组件路径进行匹配，识别出受影响的组件。对于每个受影响的组件，工作流会使用配置的认证令牌执行 subtree push 操作，将主仓库中的改动推送到组件仓库的对应分支。

工作流会：

1. checkout 主仓库完整历史
2. 根据 `github.event.before..github.sha` 计算本次 push 修改过的文件
3. 从 `repos.csv` 提取所有 `target_dir`
4. 找出受影响的组件目录
5. 使用 `SUBTREE_PUSH_TOKEN` 配置认证
6. 对每个变更组件执行 `python3 scripts/repo/repo.py push <repo_name> -b <branch>`

默认分支是 `dev`，但手动触发时可以覆盖。

#### 6.1.3 认证

主仓库需要配置一个名为 `SUBTREE_PUSH_TOKEN` 的 Secret 来完成跨仓库推送认证。由于存在跨组织组件仓库，无法依赖主仓库默认的 `GITHUB_TOKEN` 完成跨仓库推送，因此必须使用具有跨仓库访问权限的 Personal Access Token。

配置要求：
- Secret 名称：`SUBTREE_PUSH_TOKEN`
- 类型：Classic Personal Access Token
- 权限：至少包含 `repo`

#### 6.1.4 推送策略

当前默认策略是主仓库修改组件代码后自动推送到组件仓库的 `dev` 分支，由组件仓库维护者在独立仓库中继续验证、整理和合并。之所以默认推到 `dev` 而不是直接推到组件仓库 `main`，是为了避免主仓库改动直接影响组件仓库的稳定分支，给组件仓库保留审核和测试空间，同时兼容不同组织、不同维护节奏的组件管理需求。

#### 6.1.5 完整流程示例

下面的示例展示“开发者先在主仓库修改组件，再由主仓库自动推到独立组件仓库 `dev`”的完整处理流程。

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│              场景 A：主仓库修改组件 -> 自动推送到组件仓库 dev               │
└─────────────────────────────────────────────────────────────────────────────┘

时间线    主仓库 (tgoskits)                GitHub Actions                 组件仓库
  │
  │     ┌──────────────────────────────────────┐
  T1    │ 开发者修改主仓库中的组件代码          │
  │     │ 例如：components/axcpu/src/...       │
  │     │ git commit && git push origin main   │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │     ┌──────────────────────────────────────┐
  T2    │ 触发 .github/workflows/push.yml      │
  │     │ 事件：push(main) 或 workflow_dispatch│
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │     ┌──────────────────────────────────────┐
  T3    │ 检测本次 push 改动的文件              │
  │     │ git diff before..sha                 │
  │     │ 从 repos.csv 匹配受影响的 target_dir │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │     ┌──────────────────────────────────────┐
  T4    │ 配置跨仓库认证                        │
  │     │ 使用 SUBTREE_PUSH_TOKEN              │
  │     │ 配置 git credential helper           │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       ▼
  │     ┌──────────────────────────────────────┐
  T5    │ 对每个受影响组件执行 subtree push     │
  │     │ python3 scripts/repo/repo.py push    │
  │     │   <repo_name> -b dev                 │
  │     │                                      │
  │     │ 底层等价于：                         │
  │     │ git subtree push --prefix=<dir> ...  │
  │     └─────────────────┬────────────────────┘
  │                       │
  │                       │ push 到组件仓库 dev
  │                       ▼
  │                                             ┌───────────────────────────┐
  T6                                          ─►│ 组件仓库 dev 分支收到更新 │
  │                                             │ 维护者可在独立仓库继续    │
  │                                             │ 测试、整理、补充提交      │
  │                                             └────────────┬──────────────┘
  │                                                          │
  │                                                          ▼
  │                                             ┌───────────────────────────┐
  T7                                          ─►│ 组件仓库维护者合并到 main │
  │                                             │ 或 master                 │
  │                                             └───────────────────────────┘
  ▼
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

## 7. 开发场景示例

### 7.1 当前 CI 拓扑

当前的双向自动同步机制由两条独立的 CI 链路组成：主仓库的 `push.yml` 负责将主仓库中的组件改动推送到组件仓库的 `dev` 分支；组件仓库的 `push.yml`（由主仓库的 `scripts/push.yml` 复制而来）负责在组件仓库主线更新时向主仓库发起同步 PR。这两条链路共同确保了主仓库和各组件仓库之间的代码能够及时、准确地双向同步。

```text
主仓库 dev 分支
  └─ .github/workflows/push.yml
       └─ git subtree push
            └─ 组件仓库 dev

组件仓库 main/master
  └─ .github/workflows/push.yml  (由 scripts/push.yml 复制而来)
       └─ checkout 主仓库
       └─ git subtree pull --prefix=<target_dir> <repo> <sha>
       └─ push sync branch
       └─ create PR to 主仓库 main
```

这两条链路合起来，构成当前实际生效的双向同步机制。

### 7.2 端到端闭环示意

下面这张图把“主仓库改组件 -> 推到组件仓库 dev -> 组件仓库合并到 main -> 反向给主仓库提 PR -> 主仓库合并”的完整闭环串起来。

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                           双向同步完整闭环                                  │
└─────────────────────────────────────────────────────────────────────────────┘

主仓库 tgoskits                                                    组件仓库
┌──────────────────────────────┐                              ┌──────────────────┐
│ dev                          │                              │ dev              │
│ 修改 components/<name>/      │                              │ 接收主仓库推送   │
└──────────────┬───────────────┘                              └────────┬─────────┘
               │                                                       │
               │ push.yml                                              │ 维护者测试/整理
               │ subtree push                                          │
               ▼                                                       ▼
        ┌──────────────┐                                       ┌──────────────────┐
        │ 自动推到 dev │                                       │ main / master    │
        └──────┬───────┘                                       │ 合并独立仓库主线 │
               │                                               └────────┬─────────┘
               │                                                        │
               │                                      组件仓库 push.yml  │
               │                                      subtree pull + PR │
               │                                                        ▼
               │                                               ┌──────────────────┐
               └──────────────────────────────────────────────►│ 主仓库同步分支   │
                                                               │ subtree-sync/... │
                                                               └────────┬─────────┘
                                                                        │
                                                                        │ create PR
                                                                        ▼
                                                               ┌──────────────────┐
                                                               │ 主仓库 PR 到 main│
                                                               │ 评审 / 测试 / 合并│
                                                               └────────┬─────────┘
                                                                        │
                                                                        ▼
                                                               ┌──────────────────┐
                                                               │ main 更新完成     │
                                                               └──────────────────┘
```

### 7.3 典型场景

#### 7.3.1 在主仓库里改了组件代码

当你在主仓库的 `components/` 目录下修改了某个组件的代码并提交推送到 `dev` 分支后，主仓库的 `push.yml` 工作流会自动识别受影响的组件，并将对应的 subtree 改动推送到组件仓库的 `dev` 分支。整个流程完全自动化，开发者无需手动执行任何同步操作。

典型流程：

1. 在 `components/<name>/` 修改代码
2. 提交并推送到主仓库 `dev`
3. 主仓库 `push.yml` 自动识别受影响组件
4. 自动把对应 subtree 推到组件仓库 `dev`

#### 7.3.2 在组件仓库里改了主线代码

当组件仓库的 `main` 或 `master` 分支合入新的提交后，组件仓库的 workflow 会自动 checkout 主仓库并执行精确到当前 SHA 的 `git subtree pull` 操作，然后自动向主仓库创建或更新同步 PR。主仓库维护者只需要评审、测试并合并该 PR 即可完成同步。

典型流程：

1. 在组件仓库 `main` 或 `master` 合入新提交
2. 组件仓库 workflow 自动 checkout 主仓库
3. 自动执行一次精确到当前 SHA 的 `git subtree pull`
4. 自动向主仓库创建或更新同步 PR

#### 7.3.3 组件仓库远端已前进，主仓库要强制回推

当组件仓库的远端分支已经前进，而主仓库需要强制回推改动时，可以手动使用 `python3 scripts/repo/repo.py push <repo_name> -f` 命令。但强制推送意味着会重写组件仓库目标分支的历史，建议只在明确知道该分支是主仓库控制的集成分支、已经确认组件仓库远端改动不需要保留、且团队已经对这次历史覆盖达成一致的情况下使用。

## 8. 注意事项

### 8.1 `repos.csv` 是同步的事实来源

无论是手工命令还是 CI 自动化操作，组件 URL、路径、推荐分支等关键信息都来自 `repos.csv` 配置文件。如果该文件中的记录出现错误，会直接影响 `repo.py list`、`repo.py pull`、`repo.py push` 等手工命令的执行，以及主仓库自动推送和组件仓库自动创建 PR 等自动化流程的正常运行。因此，维护 `repos.csv` 的准确性对于整个同步系统至关重要。

### 8.2 `push` 和 `pull` 的默认分支规则不同

`pull` 操作更偏向于跟踪组件仓库配置的分支，会优先使用 `repos.csv` 中记录的分支或自动检测组件仓库的默认分支；而 `push` 操作更偏向于统一推到组件仓库的 `dev` 分支作为集成分支，以避免直接修改组件仓库的稳定分支。在实际使用中需要注意两者的差异，不要把这两种操作的分支策略混为一谈。

## 9. 分支管理

TGOSKits 主仓库采用三层层级的分支管理策略：`main` 作为稳定发布分支、`dev` 作为集成分支、以及开发者个人使用的功能分支。这套策略与第 6 节描述的双向自动同步机制紧密配合。

### 9.1 分支总览

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
    │  根据需要发布版本
    │  接收组件仓库反向同步 PR
    └──────────────────────────────────────
```

### 9.2 main 分支 — 稳定发布分支

`main` 分支是仓库的稳定基线，始终处于可发布状态。该分支带有分支保护规则（见下方），确保只有经过充分验证的代码才能进入。

核心规则：

- **每周合并**：定期从 `dev` 分支合并最新内容，合并前应确认 CI 测试通过
- **版本发布**：根据项目进度在 `main` 分支上打版本 tag（如 `v0.1.0`），触发 release 流程
- **分支保护**：禁止直接 push，所有代码变更必须通过 PR 进入
- **接收同步 PR**：组件仓库通过其 `push.yml` 向主仓库 `main` 发起的 subtree 同步 PR 也在此合并

### 9.3 dev 分支 — 集成分支

`dev` 分支是日常开发的主战场，所有功能开发和 bug 修复最终都汇聚到这里。

核心规则：

- **实时汇聚**：所有经 PR 验证的功能分支合并到 `dev`，确保开发进度持续集成
- **CI 测试**：`dev` 分支上的每次 push 都触发 CI 测试（`.github/workflows/test.yml` 等），确保代码质量
- **自动同步**：`dev` 分支上的 push 会触发 `push.yml`，自动将组件改动推送到各组件仓库的 `dev` 分支（详见第 6.1 节）
- **作为合并源**：`main` 分支定期从 `dev` 合并，因此 `dev` 应保持可编译、可测试的状态

### 9.4 功能分支 — 开发者个人分支

开发者基于 `dev` 分支创建功能分支进行开发。功能分支的命名建议遵循以下约定：

| 类型 | 命名格式 | 示例 |
|------|----------|------|
| 新功能 | `feature/<描述>` | `feature/vm-pause-resume` |
| Bug 修复 | `fix/<描述>` | `fix/pl011-uart-overflow` |
| 重构 | `refactor/<描述>` | `refactor/axvm-crate-split` |
| 文档 | `docs/<描述>` | `docs/branch-policy` |
| 实验性 | `experiment/<描述>` | `experiment/riscv-smp` |

### 9.5 PR 规则与合并流程

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

### 9.6 与自动同步的关系

分支管理策略与第 6 节描述的双向自动同步机制紧密配合，具体关系如下：

- **主仓库 `dev` → 组件仓库 `dev`**：`push.yml` 在 `dev` 分支 push 时触发，自动推送 subtree 改动
- **组件仓库 `main` → 主仓库 `main`**：组件仓库的 `push.yml` 在组件仓库主线更新时触发，向主仓库 `main` 发起同步 PR
- **主仓库 `main` 发布**：`main` 分支上的版本 tag 触发 release 流程，发布到 crates.io

```text
功能分支 ──PR──► dev ──push.yml──► 组件仓库 dev
                      │
                   定期合并
                      │
                      ▼
              main ◄──PR── 组件仓库 main (反向同步)
              │
           版本 tag
              │
              ▼
          crates.io 发布
```
