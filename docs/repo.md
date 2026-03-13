# TGOSKits 仓库管理指南

本文档介绍 TGOSKits 主仓库的组件管理机制以及基于 Git Subtree 的双向同步方案。

## 目录

- [概述](#概述)
- [快速开始](#快速开始)
- [配置文件](#配置文件)
- [命令参考](#命令参考)
- [工作流程](#工作流程)
- [故障排查](#故障排查)

---

## 概述

### 什么是 Git Subtree

Git Subtree 是一种将外部仓库嵌入到主仓库特定目录的技术，相比 Git Submodule，它有以下优势：

- **完整历史保留**：保留组件的完整提交历史
- **无需额外操作**：克隆主仓库时自动获取所有组件
- **独立开发**：组件可以独立开发和发布

### 核心概念

```
主仓库 (tgoskits)  ←────────────────→  组件仓库
     │                                   │
     ├─ repo.py pull ───────────────→ 拉取更新
     │                                   │
     └─ repo.py push ←────────────── 推送修改
```

---

## 快速开始

### 环境要求

- Python 3.6+
- Git 2.0+

### 常用命令

```bash
# 列出所有组件
python3 scripts/repo/repo.py list

# 拉取所有组件更新
python3 scripts/repo/repo.py pull --all

# 推送指定组件
python3 scripts/repo/repo.py push <repo_name>

# 查看帮助
python3 scripts/repo/repo.py --help
```

---

## 配置文件

### repos.csv 格式

组件配置位于 `scripts/repo/repos.csv`，使用 CSV 格式：

```csv
url,branch,target_dir,category,description
https://github.com/arceos-hypervisor/arm_vcpu,,components/arm_vcpu,Hypervisor,
https://github.com/arceos-org/arceos,dev,os/arceos,OS,ArceOS
https://github.com/arceos-org/axcpu,dev,components/axcpu,ArceOS,
```

| 字段 | 说明 | 必填 |
|------|------|------|
| `url` | 仓库 URL | ✅ |
| `branch` | 分支名（留空自动检测 main/master） | ❌ |
| `target_dir` | 本地目标目录 | ✅ |
| `category` | 分类标签（如 Hypervisor, ArceOS） | ❌ |
| `description` | 组件描述 | ❌ |

### 当前组件分类

| 分类 | 说明 | 示例 |
|------|------|------|
| **OS** | 操作系统项目 | arceos, axvisor, StarryOS |
| **Hypervisor** | 虚拟化组件 | arm_vcpu, arm_vgic, axvm |
| **ArceOS** | ArceOS 框架组件 | axcpu, axsched, axerrno |
| **Starry** | StarryOS 组件 | starry-process, starry-signal |
| **rCore** | rCore 组件 | bitmap-allocator |

---

## 命令参考

### list - 列出仓库

列出 CSV 文件中配置的所有仓库。

```bash
# 列出所有仓库
python3 scripts/repo/repo.py list

# 按分类过滤
python3 scripts/repo/repo.py list --category Hypervisor
python3 scripts/repo/repo.py list --category ArceOS
```

输出示例：
```
Name                      Category        Target                               Branch    
-------------------------------------------------------------------------------------
arceos                    OS              os/arceos                            dev       
axvisor                   OS              os/axvisor                                     
arm_vcpu                  Hypervisor      components/arm_vcpu                           
```

### add - 添加组件

添加新的 subtree 组件。

```bash
python3 scripts/repo/repo.py add \
  --url <repo_url> \
  --target <target_dir> \
  [--branch <branch>] \
  [--category <category>] \
  [--description <description>]
```

**参数说明：**
- `--url`：仓库 URL（必填）
- `--target`：本地目标目录（必填）
- `--branch`：分支名，留空自动检测
- `--category`：分类标签
- `--description`：组件描述

**示例：**
```bash
# 添加组件（自动检测分支）
python3 scripts/repo/repo.py add \
  --url https://github.com/org/new-component \
  --target components/new-component \
  --category Hypervisor

# 添加组件（指定分支）
python3 scripts/repo/repo.py add \
  --url https://github.com/org/new-component \
  --target components/new-component \
  --branch dev \
  --category Hypervisor
```

**注意：**
- 工作目录必须干净（无未提交更改）
- 如果 URL 或 target_dir 已存在，会报错

### remove - 移除组件

从 CSV 配置中移除组件。

```bash
python3 scripts/repo/repo.py remove <repo_name> [--remove-dir]
```

**参数说明：**
- `repo_name`：仓库名称（从 URL 提取，如 `arm_vcpu`）
- `--remove-dir`：同时删除本地目录

**示例：**
```bash
# 仅从 CSV 中移除
python3 scripts/repo/repo.py remove old-component

# 同时删除目录
python3 scripts/repo/repo.py remove old-component --remove-dir
```

### pull - 拉取更新

从远程仓库拉取更新到主仓库。

```bash
python3 scripts/repo/repo.py pull <repo_name> [-b <branch>] [--force]
python3 scripts/repo/repo.py pull --all
```

**参数说明：**
- `repo_name`：仓库名称
- `--all`：拉取所有仓库
- `-b, --branch`：指定分支（覆盖 CSV 配置）
- `--force`：强制模式，移除并重新添加 subtree

**示例：**
```bash
# 拉取指定组件
python3 scripts/repo/repo.py pull arm_vcpu

# 拉取指定分支
python3 scripts/repo/repo.py pull arm_vcpu -b dev

# 拉取所有组件
python3 scripts/repo/repo.py pull --all

# 强制拉取（解决冲突）
python3 scripts/repo/repo.py pull arm_vcpu --force
```

### push - 推送更改

将主仓库中的组件更改推送到远程仓库。

```bash
python3 scripts/repo/repo.py push <repo_name> [-b <branch>]
python3 scripts/repo/repo.py push --all
```

**参数说明：**
- `repo_name`：仓库名称
- `--all`：推送所有仓库
- `-b, --branch`：指定分支（覆盖 CSV 配置）

**示例：**
```bash
# 推送指定组件
python3 scripts/repo/repo.py push arm_vcpu

# 推送到指定分支
python3 scripts/repo/repo.py push arm_vcpu -b dev

# 推送所有组件
python3 scripts/repo/repo.py push --all
```

### branch - 切换分支

切换组件到不同的分支。

```bash
python3 scripts/repo/repo.py branch <repo_name> <new_branch>
```

**示例：**
```bash
# 切换到 dev 分支
python3 scripts/repo/repo.py branch arm_vcpu dev

# 切换回 main 分支
python3 scripts/repo/repo.py branch arm_vcpu main
```

此命令会：
1. 从新分支拉取更新
2. 更新 CSV 配置文件

### init - 初始化 subtrees

从 CSV 文件批量添加所有 subtrees。

```bash
python3 scripts/repo/repo.py init -f <csv_file>
```

**示例：**
```bash
# 从默认 CSV 初始化
python3 scripts/repo/repo.py init -f scripts/repo/repos.csv

# 从自定义 CSV 初始化
python3 scripts/repo/repo.py init -f /path/to/repos.csv
```

**注意：**
- 已存在的目录会跳过
- 工作目录必须干净

---

## 工作流程

### 从组件仓库同步到主仓库

当组件仓库有更新时：

```bash
# 1. 拉取指定组件更新
python3 scripts/repo/repo.py pull arm_vcpu

# 2. 或拉取所有组件更新
python3 scripts/repo/repo.py pull --all

# 3. 推送到主仓库
git push origin main
```

### 从主仓库同步到组件仓库

当在主仓库中修改了组件代码：

```bash
# 1. 在主仓库中修改组件代码
vim components/arm_vcpu/src/lib.rs

# 2. 提交更改
git add components/arm_vcpu/
git commit -m "feat: update arm_vcpu"

# 3. 推送到组件仓库
python3 scripts/repo/repo.py push arm_vcpu

# 4. 推送到指定分支
python3 scripts/repo/repo.py push arm_vcpu -b dev
```

### 添加新组件

```bash
# 1. 添加组件
python3 scripts/repo/repo.py add \
  --url https://github.com/org/new-component \
  --target components/new-component \
  --category Hypervisor

# 2. 验证
python3 scripts/repo/repo.py list --category Hypervisor

# 3. 推送主仓库更改
git add scripts/repo/repos.csv
git commit -m "chore: add new-component"
git push origin main
```

### 切换组件分支

```bash
# 切换到开发分支
python3 scripts/repo/repo.py branch arm_vcpu dev

# 验证
python3 scripts/repo/repo.py list | grep arm_vcpu
```

---

## 故障排查

### 推送失败：non-fast-forward

**错误信息：**
```
! [rejected] ... -> dev (non-fast-forward)
error: failed to push some refs
```

**原因：** 远程分支有新提交

**解决方案：**
```bash
# 方案1：先拉取再推送
python3 scripts/repo/repo.py pull arm_vcpu -b dev
python3 scripts/repo/repo.py push arm_vcpu -b dev

# 方案2：强制推送（谨慎使用）
# 需要手动执行 git subtree push
```

### 拉取失败：冲突

**原因：** 主仓库和组件仓库都有修改

**解决方案：**
```bash
# 方案1：使用强制模式（优先使用远程更改）
python3 scripts/repo/repo.py pull arm_vcpu --force

# 方案2：手动解决冲突
git subtree pull --prefix=components/arm_vcpu https://github.com/... main
# 解决冲突后
git add .
git commit -m "resolve conflicts"
```

### 工作目录不干净

**错误信息：**
```
Working tree has uncommitted changes.
Please commit or stash your changes before adding a subtree.
```

**解决方案：**
```bash
# 查看状态
git status

# 提交或暂存更改
git add .
git commit -m "your message"
# 或
git stash
```

### 组件未找到

**错误信息：**
```
Error: Repository 'xxx' not found
```

**解决方案：**
```bash
# 检查 CSV 配置
python3 scripts/repo/repo.py list

# 确认仓库名称正确（从 URL 提取的最后部分）
# 例如：https://github.com/org/arm_vcpu → arm_vcpu
```

### 分支自动检测失败

**现象：** 自动检测的分支不正确

**解决方案：**
```bash
# 手动指定分支
python3 scripts/repo/repo.py pull arm_vcpu -b dev
python3 scripts/repo/repo.py push arm_vcpu -b dev

# 或更新 CSV 配置中的 branch 字段
```

---

## 最佳实践

### 分支管理

| 分支 | 用途 | 推送命令 |
|------|------|----------|
| `main` | 稳定版本 | `repo.py push <name> -b main` |
| `dev` | 开发版本 | `repo.py push <name> -b dev` |
| `feature/*` | 功能开发 | `repo.py push <name> -b feature/xxx` |

### 定期维护

```bash
# 定期检查组件状态
python3 scripts/repo/repo.py list

# 定期拉取所有更新
python3 scripts/repo/repo.py pull --all

# 清理不需要的 remote
git remote prune origin
```

### 操作前检查

```bash
# 1. 确保工作目录干净
git status

# 2. 查看组件列表
python3 scripts/repo/repo.py list

# 3. 执行操作
python3 scripts/repo/repo.py pull <name>
```

---

## 相关文件

| 文件 | 说明 |
|------|------|
| [scripts/repo/repo.py](../scripts/repo/repo.py) | Git Subtree 管理脚本 |
| [scripts/repo/repos.csv](../scripts/repo/repos.csv) | 组件仓库配置文件 |
| [README.md](../README.md) | 项目主文档 |

## 参考资料

- [Git Subtree 文档](https://git-scm.com/book/en/v2/Git-Tools-Advanced-Merging#_subtree_merge)
- [Git Subtree 参考](https://www.atlassian.com/git/tutorials/git-subtree)
