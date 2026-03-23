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

```bash
python3 scripts/repo/repo.py remove old-component
python3 scripts/repo/repo.py remove old-component --remove-dir
```

### 3.3 切换组件分支

```bash
python3 scripts/repo/repo.py branch arm_vcpu dev
python3 scripts/repo/repo.py branch arm_vcpu main
```

该命令会在同步成功后更新 `repos.csv` 中对应组件的 `branch` 字段。

### 3.4 批量初始化

```bash
python3 scripts/repo/repo.py init -f scripts/repo/repos.csv
```

适合新环境首次批量拉起所有 subtree。

## 4. 分支解析规则

### 4.1 `add` / `pull` 的默认分支

当 `repos.csv` 的 `branch` 为空，且命令行也没有显式传入 `-b/--branch` 时，`repo.py` 会自动检测组件仓库默认分支，逻辑为：

1. 优先尝试 `main`
2. 再尝试 `master`
3. 再读取 remote 的 `HEAD branch`
4. 最后兜底为 `main`

这套逻辑主要用于：

- `repo.py add`
- `repo.py pull`
- `repo.py list` 中的分支展示

### 4.2 `push` 的默认分支

`repo.py push` 与 `pull` 不同。

当前实现中：

- 如果显式传入 `-b/--branch`，则推到指定分支
- 如果未传入分支，则默认推到组件仓库的 `dev` 分支

例如：

```bash
python3 scripts/repo/repo.py push axcpu
```

等价于：

```bash
python3 scripts/repo/repo.py push axcpu -b dev
```

这是为了让主仓库向组件仓库同步时，默认走组件仓库的集成分支，而不是直接改写对方 `main`。

## 5. 手动同步

### 5.1 从组件仓库同步到主仓库

```bash
python3 scripts/repo/repo.py pull arm_vcpu
python3 scripts/repo/repo.py pull arm_vcpu -b dev
python3 scripts/repo/repo.py pull --all
```

`pull` 的行为：

1. 如果组件目录尚未加入主仓库，则自动执行 `add`
2. 如果未指定分支，则优先使用 `repos.csv` 里的 `branch`
3. 若 `branch` 为空，则自动检测远程默认分支
4. 执行 `git subtree pull`

#### 5.1.1 强制模式

```bash
python3 scripts/repo/repo.py pull arm_vcpu --force
```

适用场景：

- 组件仓库历史被重写
- 合并冲突难以直接处理
- 需要重建本地 subtree

### 5.2 从主仓库同步到组件仓库

```bash
python3 scripts/repo/repo.py push arm_vcpu
python3 scripts/repo/repo.py push arm_vcpu -b dev
python3 scripts/repo/repo.py push arm_vcpu -b release/x.y
python3 scripts/repo/repo.py push arm_vcpu -f
python3 scripts/repo/repo.py push --all
```

`push` 的行为：

1. 检查目标组件目录已存在
2. 若未传入分支，则默认推到 `dev`
3. 执行 `git subtree push`
4. 如果使用 `-f/--force`，则通过带 `+` 的 refspec 强制推送到远端分支

说明：

- `git subtree push` 不支持单独的 `--force` 参数
- 强制推送是通过 refspec 形式实现的，例如 `+dev`
- 如果组件仓库远端已经前进，通常应先做同步确认，再决定是否 `--force`

## 6. 自动同步方案

当前仓库采用两条自动同步链路：

1. 主仓库 `main` 收到修改后，自动把改动推到组件仓库的 `dev` 分支
2. 组件仓库 `main` 或 `master` 收到修改后，自动向主仓库 `main` 发起 subtree 同步 PR

### 6.1 从主仓库到组件仓库

主仓库使用 [push.yml](/home/zcs/WORKSPACE/tgoskits/.github/workflows/push.yml)。

#### 6.1.1 触发方式

- `push` 到主仓库 `main`
- 手动触发 `workflow_dispatch`

#### 6.1.2 工作流行为

工作流会：

1. checkout 主仓库完整历史
2. 根据 `github.event.before..github.sha` 计算本次 push 修改过的文件
3. 从 `repos.csv` 提取所有 `target_dir`
4. 找出受影响的组件目录
5. 使用 `SUBTREE_PUSH_TOKEN` 配置认证
6. 对每个变更组件执行 `python3 scripts/repo/repo.py push <repo_name> -b <branch>`

默认分支是 `dev`，但手动触发时可以覆盖。

#### 6.1.3 认证

主仓库需要配置：

- Secret 名称：`SUBTREE_PUSH_TOKEN`
- 类型：Classic Personal Access Token
- 权限：至少包含 `repo`

因为存在跨组织组件仓库，不能依赖主仓库默认的 `GITHUB_TOKEN` 完成跨仓库推送。

#### 6.1.4 推送策略

当前默认策略是：

- 主仓库修改组件代码
- 自动推送到组件仓库的 `dev` 分支
- 由组件仓库维护者在独立仓库中继续验证、整理和合并

之所以默认推到 `dev`，而不是直接推到组件仓库 `main`，是为了：

- 避免主仓库改动直接影响组件仓库稳定分支
- 给组件仓库保留审核和测试空间
- 兼容不同组织、不同维护节奏的组件

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

组件仓库使用模板 [scripts/push.yml](/home/zcs/WORKSPACE/tgoskits/scripts/push.yml)。

将它复制到组件仓库：

```bash
cp scripts/push.yml <component-repo>/.github/workflows/push.yml
```

#### 6.2.1 触发方式

- `push` 到组件仓库 `main`
- `push` 到组件仓库 `master`
- 手动触发 `workflow_dispatch`

之所以监听 `main/master`，是为了兼容不同组件仓库的默认主分支命名，同时避免主仓库推到组件 `dev` 后形成自动循环。

#### 6.2.2 工作流行为

组件仓库中的该 workflow 会：

1. 使用 `PARENT_REPO_TOKEN` checkout 主仓库 `main`
2. 从主仓库的 `scripts/repo/repos.csv` 中按当前组件仓库 URL 查找 `target_dir`
3. 在主仓库中创建或重置同步分支，例如 `subtree-sync/<repo>-main`
4. 执行：

```bash
git subtree pull --prefix=<target_dir> <component_repo_url> <commit_sha>
```

5. 如果没有实际变更，则跳过
6. 如果有变更，则将同步分支推到主仓库
7. 创建或更新一个指向主仓库 `main` 的 PR

#### 6.2.3 认证

组件仓库需要配置：

- Secret 名称：`PARENT_REPO_TOKEN`
- 类型：Classic Personal Access Token
- 权限：至少包含 `repo`

该 token 需要能够：

- checkout 主仓库
- 向主仓库推送同步分支
- 在主仓库创建 PR

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

```text
主仓库 main
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

主仓库 tgoskits                                              组件仓库
┌──────────────────────────────┐                            ┌──────────────────┐
│ main                         │                            │ dev              │
│ 修改 components/<name>/      │                            │ 接收主仓库推送   │
└──────────────┬───────────────┘                            └────────┬─────────┘
               │                                                       │
               │ push.yml                                              │ 维护者测试/整理
               │ subtree push                                          │
               ▼                                                       ▼
        ┌──────────────┐                                       ┌──────────────────┐
        │ 自动推到 dev │                                       │ main / master    │
        └──────┬───────┘                                       │ 合并独立仓库主线 │
               │                                               └────────┬─────────┘
               │                                                        │
               │                                      组件仓库 push.yml │
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

典型流程：

1. 在 `components/<name>/` 修改代码
2. 提交并推送到主仓库 `main`
3. 主仓库 `push.yml` 自动识别受影响组件
4. 自动把对应 subtree 推到组件仓库 `dev`

#### 7.3.2 在组件仓库里改了主线代码

典型流程：

1. 在组件仓库 `main` 或 `master` 合入新提交
2. 组件仓库 workflow 自动 checkout 主仓库
3. 自动执行一次精确到当前 SHA 的 `git subtree pull`
4. 自动向主仓库创建或更新同步 PR

#### 7.3.3 组件仓库远端已前进，主仓库要强制回推

可以手动使用：

```bash
python3 scripts/repo/repo.py push <repo_name> -f
```

但强推意味着会重写组件仓库目标分支历史，建议只在以下情况下使用：

- 明确知道该分支是主仓库控制的集成分支
- 已经确认组件仓库远端改动不需要保留
- 团队已经对这次历史覆盖达成一致

## 8. 注意事项

### 8.1 `repos.csv` 是同步的事实来源

无论是手工命令还是 CI，组件 URL、路径、推荐分支都来自 `repos.csv`。如果其中记录错误，会直接影响：

- `repo.py list`
- `repo.py pull`
- `repo.py push`
- 主仓库自动推送
- 组件仓库自动创建 PR

### 8.2 `push` 和 `pull` 的默认分支规则不同

- `pull` 更偏向“跟踪组件仓库配置的分支”
- `push` 更偏向“统一推到组件仓库 `dev` 作为集成分支”

不要把两者混为一谈。
