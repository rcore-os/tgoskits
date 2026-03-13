# TGOSKits 仓库管理指南

本文档介绍 TGOSKits 主仓库的架构设计、组件管理机制以及基于 Git Subtree 的双向同步方案。

## 目录

- [仓库概述](#仓库概述)
  - [项目简介](#项目简介)
  - [仓库结构](#仓库结构)
  - [核心特性](#核心特性)
- [Git Subtree 基础](#git-subtree-基础)
  - [什么是 Git Subtree](#什么是-git-subtree)
  - [为什么选择 Subtree](#为什么选择-subtree)
  - [基本概念](#基本概念)
- [组件管理](#组件管理)
  - [组件列表](#组件列表)
  - [组件结构](#组件结构)
  - [组件生命周期](#组件生命周期)
- [同步方案](#同步方案)
  - [方案概述](#方案概述-1)
  - [架构设计](#架构设计)
  - [工具脚本](#工具脚本)
  - [快速开始](#快速开始)
  - [详细配置](#详细配置)
  - [使用指南](#使用指南)
  - [工作流程](#工作流程)
  - [故障排查](#故障排查)
  - [最佳实践](#最佳实践)

---

## 仓库概述

### 项目简介

TGOSKits 是一个面向操作系统开发工具包的集成仓库，通过 Git Subtree 技术将多个独立的组件仓库整合到一个统一的主仓库中，同时保留每个组件的完整开发历史。

**主要目标**：
- 🎯 **统一管理**：在单一仓库中管理所有操作系统相关组件
- 📜 **保留历史**：完整保留每个组件的独立开发历史和提交记录
- 🔄 **双向同步**：支持主仓库和组件仓库之间的双向代码同步
- 🚀 **独立开发**：组件可以独立开发、测试和发布
- 🤝 **协作友好**：支持多人协作和自动化工作流

### 仓库结构

```
tgoskits/                              # 主仓库根目录
├── components/                        # 组件目录（可复用的库和模块）
│   ├── arm_vcpu/                     # ARM 虚拟 CPU 支持
│   ├── arm_vgic/                     # ARM 虚拟 GIC 控制器
│   ├── axvm/                         # 虚拟机抽象层
│   ├── axvisor/                      # Hypervisor 核心
│   ├── axaddrspace/                  # 地址空间管理
│   ├── axdevice/                     # 设备抽象层
│   └── ... (更多组件)
│
├── os/                               # 操作系统项目
│   ├── arceos/                       # ArceOS 系统
│   ├── axvisor/                      # Axvisor Hypervisor
│   └── StarryOS/                     # StarryOS
│
├── scripts/                          # 管理脚本
│   ├── repos.list                    # 组件仓库配置
│   ├── push.sh                       # 推送脚本
│   ├── pull.sh                       # 拉取脚本
│   ├── check.sh                      # 检查脚本
│   └── repos.sh                      # 仓库管理脚本
│
├── docs/                             # 文档目录
│   └── repo.md                       # 本文档
│
└── .github/workflows/                # GitHub Actions
    ├── pull.yml                      # 自动拉取工作流
    └── push.yml                      # 自动推送工作流
```

### 核心特性

#### 1. 历史保留

使用 Git Subtree 而非 Git Submodule 的关键优势是**完整保留组件的提交历史**：

```bash
# 查看组件的完整历史（包括合并前的提交）
git log --follow -- components/arm_vcpu/src/lib.rs

# 查看组件的 subtree 合并历史
git log --oneline --grep="subtree" --grep="arm_vcpu"
```

#### 2. 独立开发

每个组件都可以在独立仓库中进行开发和维护：

- 组件仓库：`https://github.com/arceos-hypervisor/arm_vcpu`
- 可以独立提交、测试、发布
- 有独立的 Issue 追踪和 Pull Request 流程

#### 3. 双向同步

支持两种同步方向：

```
主仓库 (tgoskits)  ←────────────────→  组件仓库
     │                                   │
     ├─ scripts/push.sh ─────────────→ 推送修改
     │                                   │
     └─ scripts/pull.sh ←───────────── 拉取更新
```

#### 4. 自动化工作流

通过 GitHub Actions 实现自动化同步：

- 组件仓库推送 → 自动触发主仓库拉取
- 主仓库修改 → 手动或自动推送到组件仓库
- 支持批量操作和定时检查

---

## 组件管理

### 组件列表

当前仓库管理的所有组件定义在 [scripts/repos.list](../scripts/repos.list) 文件中。

**组件分类**：

#### Hypervisor 核心组件（arceos-hypervisor 组织）

| 组件 | 描述 | 仓库 |
|------|------|------|
| `arm_vcpu` | ARM 虚拟 CPU 支持 | [arceos-hypervisor/arm_vcpu](https://github.com/arceos-hypervisor/arm_vcpu) |
| `arm_vgic` | ARM 虚拟 GIC 控制器 | [arceos-hypervisor/arm_vgic](https://github.com/arceos-hypervisor/arm_vgic) |
| `axvm` | 虚拟机抽象层 | [arceos-hypervisor/axvm](https://github.com/arceos-hypervisor/axvm) |
| `axvisor` | Hypervisor 核心框架 | [arceos-hypervisor/axvisor](https://github.com/arceos-hypervisor/axvisor) |
| `axaddrspace` | 地址空间管理 | [arceos-hypervisor/axaddrspace](https://github.com/arceos-hypervisor/axaddrspace) |
| `axdevice` | 设备抽象层 | [arceos-hypervisor/axdevice](https://github.com/arceos-hypervisor/axdevice) |

#### ArceOS 框架组件（arceos-org 组织）

| 组件 | 描述 | 仓库 |
|------|------|------|
| `arceos` | ArceOS 操作系统 | [arceos-org/arceos](https://github.com/arceos-org/arceos) |
| `axcpu` | CPU 抽象层 | [arceos-org/axcpu](https://github.com/arceos-org/axcpu) |
| `axsched` | 调度器框架 | [arceos-org/axsched](https://github.com/arceos-org/axsched) |
| `axconfig-gen` | 配置生成工具 | [arceos-org/axconfig-gen](https://github.com/arceos-org/axconfig-gen) |

完整列表请查看 [scripts/repos.list](../scripts/repos.list) 文件。

### 组件结构

每个组件都遵循标准的 Rust 项目结构：

```
components/arm_vcpu/
├── Cargo.toml           # 项目配置和依赖
├── README.md            # 组件说明文档
├── LICENSE              # 许可证文件
├── CHANGELOG.md         # 变更日志（可选）
├── rust-toolchain.toml  # Rust 工具链配置（可选）
├── src/                 # 源代码目录
│   ├── lib.rs          # 库入口
│   └── ...
└── tests/               # 测试代码（可选）
```

**组件要求**：
- ✅ 必须包含 `Cargo.toml` 和 `README.md`
- ✅ 建议包含 `LICENSE` 文件
- ✅ 建议维护 `CHANGELOG.md`
- ✅ 代码风格符合 Rust 规范

### 组件生命周期

#### 1. 添加新组件

**步骤 1：更新配置文件**

在 [scripts/repos.list](../scripts/repos.list) 中添加新组件：

```bash
# 格式：<仓库URL>|<分支>|<本地目录>
https://github.com/your-org/new-component|main|components/new-component
```

**步骤 2：添加 Subtree**

```bash
# 添加远程仓库
git remote add components/new-component https://github.com/your-org/new-component

# 添加 subtree（保留完整历史）
git subtree add --prefix=components/new-component components/new-component main

# 或使用脚本
scripts/repos.sh -a new-component
```

**步骤 3：验证**

```bash
# 检查组件是否正确添加
scripts/check.sh new-component

# 查看提交历史
git log --oneline components/new-component/
```

#### 2. 更新组件

**手动更新**：

```bash
# 拉取指定组件的更新
scripts/pull.sh -c new-component -b main

# 或使用 git subtree 命令
git subtree pull --prefix=components/new-component components/new-component main
```

**自动更新**：

配置组件仓库的 GitHub Actions，推送时自动通知主仓库拉取更新（详见[同步方案](#同步方案)章节）。

#### 3. 移除组件

**警告**：移除组件会删除本地目录和相关配置，操作需谨慎！

```bash
# 1. 删除组件目录
rm -rf components/new-component
git add components/new-component
git commit -m "chore: remove new-component"

# 2. 移除远程仓库
git remote remove components/new-component

# 3. 更新配置文件
# 从 scripts/repos.list 中删除对应行
```

#### 4. 组件分支管理

每个组件可以跟踪不同的分支：

```bash
# 查看组件当前使用的分支
git remote show components/arm_vcpu | grep "tracked"

# 切换组件分支
scripts/pull.sh -c arm_vcpu -b dev

# 推送到特定分支
scripts/push.sh -c arm_vcpu -b dev
```

**分支策略建议**：
- `main`：稳定版本，用于生产环境
- `dev`：开发版本，包含最新特性
- `feature/*`：功能分支，用于开发新特性
- `release/*`：发布分支，用于版本准备

---

## 同步方案

本章节详细介绍主仓库和组件仓库之间的双向自动同步机制。

### 方案概述

#### 同步方向

我们提供两种同步方向：

1. **主仓库 → 组件仓库**：使用 `scripts/push.sh` 手动推送更新
2. **组件仓库 → 主仓库**：使用 GitHub Actions 自动拉取更新

#### 核心优势

- ✅ **自动化同步**：组件仓库更新后自动触发主仓库同步
- ✅ **双向同步**：支持主仓库 ↔ 组件仓库双向更新
- ✅ **灵活控制**：支持手动触发、自动触发、批量操作
- ✅ **安全可靠**：使用 GitHub Token 认证，支持强制推送
- ✅ **易于扩展**：新组件只需简单配置即可接入

---

### 架构设计

#### 同步流程图

```
┌─────────────────┐                    ┌──────────────────┐
│  组件仓库        │                    │   主仓库          │
│ (arm_vcpu)      │                    │  (tgoskits)      │
├─────────────────┤                    ├──────────────────┤
│                 │  1. Push 到组件仓库  │                  │
│  开发者 ──────> │ ───────────────>   │                  │
│                 │                    │                  │
│                 │  2. GitHub Actions  │                  │
│                 │     触发通知        │                  │
│                 │ ───────────────>   │                  │
│                 │                    │  3. 拉取更新      │
│                 │                    │     (subtree pull)│
│                 │ <───────────────   │                  │
│                 │                    │                  │
│                 │  4. 手动推送        │                  │
│                 │ <───────────────   │  开发者          │
│                 │   (subtree push)   │                  │
└─────────────────┘                    └──────────────────┘
```

### 组件关系

```
tgoskits (主仓库)
├── arm_vcpu        → https://github.com/arceos-hypervisor/arm_vcpu
├── axvm            → https://github.com/arceos-hypervisor/axvm
├── axvisor         → https://github.com/arceos-hypervisor/axvisor
├── arceos          → https://github.com/arceos-org/arceos
├── axconfig-gen    → https://github.com/arceos-org/axconfig-gen
└── ... 更多组件见 scripts/repos.list
```

---

### 工具脚本

#### Shell 脚本（本地操作）

##### push.sh - 推送本地修改到组件仓库

将主仓库中的组件修改推送到各个组件的独立仓库。

```bash
# 推送所有有更改的组件（到 dev 分支）
scripts/push.sh -f

# 推送所有组件（包括未更改的）
scripts/push.sh -f --no-skip-unchanged

# 推送指定组件到 dev 分支
scripts/push.sh -c arm_vcpu -b dev

# 推送到指定分支
scripts/push.sh -c arm_vcpu -b main

# 强制推送（覆盖远程）
scripts/push.sh -c arm_vcpu --force

# 自动提交并推送
scripts/push.sh -c arm_vcpu -m "feat: update arm_vcpu"

# 预览操作（不实际执行）
scripts/push.sh --dry-run -f
```

**选项说明：**
- `-f, --file <file>` - 指定仓库列表文件并推送其中所有仓库（默认为 scripts/repos.list）
- `-c, --component <dir>` - 指定要推送的组件目录（需配合 -b 使用）
- `-b, --branch <branch>` - 指定推送的目标分支（默认为 dev）
- `-m, --commit <msg>` - 提交信息（如果没有提交会自动创建）
- `--force` - 强制推送（即使远程有更新也覆盖）
- `--no-skip-unchanged` - 不跳过没有更改的组件（默认跳过）
- `-d, --dry-run` - 仅显示将要执行的操作，不实际执行
- `-h, --help` - 显示帮助信息

**注意：**
- 默认推送到子仓库的 `dev` 分支
- 自动跳过没有更改的组件
- 如果遇到 `non-fast-forward` 错误，说明远程分支有更新，可以使用 `--force` 强制推送

##### pull.sh - 从组件仓库拉取更新

从各个组件的独立仓库拉取更新到主仓库。

```bash
# 拉取默认文件中所有仓库
scripts/pull.sh -f

# 拉取指定文件中所有仓库
scripts/pull.sh -f repos.list

# 拉取指定组件的指定分支
scripts/pull.sh -c arm_vcpu -b dev

# 预览拉取操作
scripts/pull.sh --dry-run -f
```

**选项说明：**
- `-f, --file <file>` - 指定仓库列表文件并拉取其中所有仓库（默认为 scripts/repos.list）
- `-c, --component <dir>` - 指定要拉取的组件目录（需配合 -b 使用）
- `-b, --branch <branch>` - 指定要拉取的分支（需配合 -c 使用）
- `-d, --dry-run` - 仅显示将要执行的操作，不实际执行
- `-h, --help` - 显示帮助信息

**注意：**
- CI/CD 自动拉取时会推送到主仓库的 `next` 分支
- 组件名支持简写（如 `arm_vcpu` 会自动识别为 `components/arm_vcpu`）

#### GitHub Actions Workflows（CI/CD）

##### 主仓库：pull.yml

**位置**：`.github/workflows/pull.yml`

**作用**：接收组件仓库的更新通知，自动拉取更新

**触发方式**：
1. 接收 `repository_dispatch` 事件（组件仓库推送）
2. 手动触发（workflow_dispatch）

##### 组件仓库：push.yml（模板）

**位置**：`scripts/push.yml`

**作用**：组件仓库推送代码时，通知主仓库拉取更新

**使用方式**：复制到组件仓库的 `.github/workflows/` 目录

---

### 快速开始

#### 1. 配置组件仓库（5 分钟）

##### 步骤 1：复制 workflow 文件

```bash
cd arm_vcpu  # 进入组件仓库
mkdir -p .github/workflows

# 复制模板文件
cp /path/to/tgoskits/scripts/push.yml .github/workflows/notify-parent.yml
```

##### 步骤 2：创建 Personal Access Token

1. 访问 https://github.com/settings/tokens/new
2. 设置 Token 名称：`tgoskits-subtree-sync`
3. 选择权限：
   - ✅ `repo` (Full control of private repositories)
   - ✅ `workflow` (Update GitHub Action workflows)
4. 点击 "Generate token"
5. **立即复制 Token**（只显示一次）

##### 步骤 3：配置 Secret

1. 进入组件仓库的 **Settings** 页面
2. 左侧菜单选择 **Secrets and variables** → **Actions**
3. 点击 **New repository secret**
4. 填写：
   - Name: `PARENT_REPO_TOKEN`
   - Value: 粘贴刚才复制的 Token
5. 点击 **Add secret**

##### 步骤 4：测试配置

```bash
# 在组件仓库中推送一个测试提交
echo "<!-- test -->" >> README.md
git add README.md
git commit -m "test: notify parent repository"
git push origin main
```

##### 步骤 5：验证

检查：
1. 组件仓库的 **Actions** 页面 - 确认 workflow 运行成功
2. 主仓库的 **Actions** 页面 - 确认收到通知并拉取更新
3. 主仓库的提交历史 - 应该看到 "Merge subtree arm_vcpu/main"

#### 2. 使用主仓库脚本

```bash
# 在主仓库中修改组件代码
vim components/arm_vcpu/src/lib.rs
git add components/arm_vcpu/src/lib.rs
git commit -m "feat: update arm_vcpu"

# 推送到组件仓库的 dev 分支（默认）
scripts/push.sh -c arm_vcpu -b dev

# 或使用自动提交
scripts/push.sh -c arm_vcpu -m "feat: update arm_vcpu"
```

---

### 详细配置

#### 主仓库配置

主仓库已经配置好了接收更新的 GitHub Actions workflow。

**文件位置**：`.github/workflows/pull.yml`

**配置要点**：
- 监听 `repository_dispatch` 事件
- 从 `scripts/repos.list` 读取组件信息
- 自动执行 `git subtree pull`

#### 组件仓库配置

##### GitHub Actions Workflow

在组件仓库中创建文件 `.github/workflows/notify-parent.yml`：

```yaml
name: Notify Parent Repository

on:
  push:
    branches:
      - main
      - dev
      - 'feature/**'
      - 'release/**'
  workflow_dispatch:

jobs:
  notify:
    runs-on: ubuntu-latest
    steps:
      - name: Get repository info
        id: repo
        run: |
          REPO_URL="${{ github.repositoryUrl }}"
          COMPONENT=$(echo "${REPO_URL}" | sed 's|.*/||' | sed 's|\.git$||')
          BRANCH="${{ github.ref_name }}"
          
          echo "component=${COMPONENT}" >> $GITHUB_OUTPUT
          echo "branch=${BRANCH}" >> $GITHUB_OUTPUT

      - name: Notify parent repository
        env:
          GITHUB_TOKEN: ${{ secrets.PARENT_REPO_TOKEN }}
        run: |
          COMPONENT="${{ steps.repo.outputs.component }}"
          BRANCH="${{ steps.repo.outputs.branch }}"
          PARENT_REPO="rcore-os/tgoskits"  # 修改为你的主仓库路径
          
          curl -X POST \
            -H "Accept: application/vnd.github.v3+json" \
            -H "Authorization: token ${GITHUB_TOKEN}" \
            https://api.github.com/repos/${PARENT_REPO}/dispatches \
            -d "{
              \"event_type\": \"subtree-update\",
              \"client_payload\": {
                \"component\": \"${COMPONENT}\",
                \"branch\": \"${BRANCH}\",
                \"commit\": \"${{ github.sha }}\",
                \"message\": \"${{ github.event.head_commit.message }}\",
                \"author\": \"${{ github.actor }}\"
              }
            }"
```

##### 自定义触发条件

如果只想在特定文件变化时触发，可以修改 `on.push.paths`：

```yaml
on:
  push:
    branches:
      - main
    paths:
      - 'src/**'        # 只在 src 目录变化时触发
      - 'Cargo.toml'    # 或 Cargo.toml 变化时
```

#### repos.list 配置

**文件位置**：`scripts/repos.list`

**格式**：`<仓库URL>|<分支>|<目标目录>`

```bash
# 示例
https://github.com/arceos-hypervisor/arm_vcpu||arm_vcpu
https://github.com/arceos-org/arceos|dev|arceos
```

**说明**：
- 第一个字段：仓库 URL
- 第二个字段：分支名（留空则自动检测）
- 第三个字段：本地目录名

---

### 使用指南

#### 推送操作（主仓库 → 组件仓库）

##### 基本使用

```bash
# 1. 在主仓库中修改组件代码
vim components/arm_vcpu/src/lib.rs

# 2. 提交更改
git add components/arm_vcpu/src/lib.rs
git commit -m "feat: update arm_vcpu"

# 3. 推送到组件仓库的 dev 分支（默认）
scripts/push.sh -c arm_vcpu -b dev
```

##### 高级用法

```bash
# 自动提交并推送（一步完成）
scripts/push.sh -c arm_vcpu -m "feat: update arm_vcpu"

# 推送到指定分支
scripts/push.sh -c arm_vcpu -b main

# 强制推送（覆盖远程）
scripts/push.sh -c arm_vcpu --force

# 推送所有有更改的组件
scripts/push.sh -f

# 推送所有组件（包括未更改的）
scripts/push.sh -f --no-skip-unchanged
```

#### 拉取操作（组件仓库 → 主仓库）

##### 手动拉取

```bash
# 拉取指定组件的指定分支
scripts/pull.sh -c arm_vcpu -b dev

# 拉取所有组件的更新
scripts/pull.sh -f

# 预览拉取操作
scripts/pull.sh --dry-run -f
```

##### 自动拉取

组件仓库推送代码后，主仓库会自动拉取更新：

1. 组件仓库推送代码
2. 触发 GitHub Actions
3. 通知主仓库
4. 主仓库自动执行 `git subtree pull`

#### 批量操作

```bash
# 批量推送所有有更改的组件
scripts/push.sh -f

# 批量拉取所有组件
scripts/pull.sh -f

# 批量检查所有组件状态
scripts/check.sh all
```

---

### 工作流程

#### 自动同步流程

```
1. 开发者在组件仓库推送代码
   ↓
2. 组件仓库的 GitHub Actions 被触发
   ↓
3. Actions 发送 repository_dispatch 事件到主仓库
   ↓
4. 主仓库的 GitHub Actions 被触发
   ↓
5. 主仓库执行 git subtree pull 拉取更新
   ↓
6. 主仓库自动提交并推送更改
```

#### 手动同步流程

##### 推送流程（主仓库 → 组件仓库）

```
主仓库修改组件代码 
  → git commit 
  → scripts/push.sh -c <component> -b <branch>
  → 组件仓库收到更新
```

##### 拉取流程（组件仓库 → 主仓库）

```
组件仓库更新
  → 手动触发或自动触发
  → 主仓库执行 scripts/pull.sh -c <component> -b <branch>
  → 主仓库收到更新
```

#### 冲突处理流程

```
1. 拉取时检测到冲突
   ↓
2. 手动解决冲突
   git add .
   git commit -m "resolve conflicts in <component>"
   ↓
3. 推送到主仓库
   git push origin main
   ↓
4. 推送到组件仓库
   scripts/push.sh -c <component> -b <branch>
```

---

### 故障排查

#### 推送失败：non-fast-forward

**原因**：远程分支有新的提交

**错误信息**：
```
! [rejected] ... -> zcs (non-fast-forward)
error: failed to push some refs
```

**解决方案**：

```bash
# 方案1：强制推送（覆盖远程）
scripts/push.sh -c <component> -b <branch> --force

# 方案2：先拉取再推送
scripts/pull.sh -c <component> -b <branch>
scripts/push.sh -c <component> -b <branch>
```

#### 拉取失败：冲突

**原因**：主仓库和组件仓库都有修改

**解决方案**：

```bash
# 手动拉取并解决冲突
scripts/pull.sh -c <component> -b <branch>

# 解决冲突
# ... 手动编辑冲突文件 ...

# 提交解决
git add .
git commit -m "resolve conflicts in <component>"
git push origin main

# 推送到组件仓库
scripts/push.sh -c <component> -b <branch>
```

#### Token 权限不足

**错误信息**：`HTTP 403: Resource not accessible by integration`

**解决方案**：
1. 确保 Token 有 `repo` 和 `workflow` 权限
2. 检查组件仓库的 Secret 配置是否正确
3. 确认 Token 未过期

#### 组件未找到

**错误信息**：`Component xxx not found in repos.list`

**解决方案**：
1. 检查主仓库的 `scripts/repos.list` 文件
2. 确认组件配置格式正确
3. 检查组件目录名是否匹配

#### GitHub Actions 失败

**可能原因**：
1. Token 权限不足
2. 网络问题
3. 配置错误

**排查步骤**：
1. 查看 Actions 日志
2. 检查 Token 配置
3. 验证 workflow 文件语法
4. 确认主仓库路径正确

---

### 最佳实践

#### 推送前检查

1. **确保修改已提交**
   ```bash
   git status
   git add .
   git commit -m "your message"
   ```

2. **预览推送操作**
   ```bash
   scripts/push.sh --dry-run -c <component> -b <branch>
   ```

3. **检查组件状态**
   ```bash
   scripts/check.sh <component>
   ```

#### 强制推送使用场景

**适合使用 `--force`**：
- 确认远程提交可以被覆盖
- 个人分支测试
- 修复错误的提交

**不适合使用 `--force`**：
- 团队协作的分支
- 重要的历史提交
- 不确定远程状态时

#### 分支管理建议

1. **开发分支**：使用 `dev` 分支进行开发
2. **主分支**：`main` 分支保持稳定
3. **发布分支**：使用 `release/**` 分支准备发布

```bash
# 推送到开发分支
scripts/push.sh -c arm_vcpu -b dev

# 合并到主分支后推送
scripts/push.sh -c arm_vcpu -b main
```

#### 定期维护

```bash
# 定期检查所有组件状态
scripts/check.sh all

# 定期拉取所有组件更新
scripts/pull.sh -f

# 清理不需要的 remote
git remote prune origin
```

#### Token 管理

1. **定期更新 Token**：建议每 3-6 个月更新一次
2. **最小权限原则**：只给予必要的权限
3. **安全存储**：不要在代码中硬编码 Token
4. **监控使用**：定期检查 Token 使用情况

---

### 附录

#### 组件仓库列表

当前配置的组件仓库：

- **arceos-hypervisor 组织**：arm_vcpu, axvm, axvisor, axaddrspace, axdevice 等
- **arceos-org 组织**：arceos, axconfig-gen, axcpu, axsched 等

完整列表见主仓库的 `scripts/repos.list` 文件。

#### 相关文件

- `scripts/push.sh` - 推送脚本
- `scripts/pull.sh` - 拉取脚本
- `scripts/repos.sh` - 仓库管理脚本
- `scripts/check.sh` - 检查脚本
- `scripts/push.yml` - 组件仓库 workflow 模板
- `.github/workflows/pull.yml` - 主仓库 workflow
- `scripts/repos.list` - 组件配置列表

#### 参考资料

- [Git Subtree 文档](https://git-scm.com/book/en/v2/Git-Tools-Advanced-Merging#_subtree_merge)
- [GitHub Actions 文档](https://docs.github.com/en/actions)
- [GitHub API 文档](https://docs.github.com/en/rest)

---

### 获取帮助

如果遇到问题，请：

1. 查看本文档的故障排查章节
2. 检查 GitHub Actions 的日志
3. 查看相关脚本的帮助信息：`scripts/push.sh --help`
4. 提交 Issue 或联系维护者
