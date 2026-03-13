# TGOSKits

TGOSKits 是一个面向操作系统开发的工具包集成仓库，通过 Git Subtree 技术将多个独立的组件仓库整合到一个统一的主仓库中，同时保留每个组件的完整开发历史。

## 特性

- 🎯 **统一管理** - 在单一仓库中管理所有操作系统相关组件
- 📜 **历史保留** - 完整保留每个组件的独立开发历史和提交记录
- 🔄 **双向同步** - 支持主仓库和组件仓库之间的双向代码同步
- 🚀 **独立开发** - 组件可以独立开发、测试和发布

## 仓库结构

```
tgoskits/
├── components/           # 可复用的库和模块
│   ├── aarch64_sysreg/  # ARM64 系统寄存器
│   ├── arm_vcpu/        # ARM 虚拟 CPU 支持
│   ├── arm_vgic/        # ARM 虚拟 GIC 控制器
│   ├── axaddrspace/     # 地址空间管理
│   ├── axdevice/        # 设备抽象层
│   ├── axvm/            # 虚拟机抽象层
│   └── ...              # 更多组件
│
├── os/                  # 操作系统项目
│   ├── arceos/          # ArceOS 系统
│   ├── axvisor/         # Axvisor Hypervisor
│   └── StarryOS/        # StarryOS
│
├── scripts/             # 管理脚本
│   └── repo/            # 仓库管理工具
│       ├── repo.py      # Git Subtree 管理脚本
│       └── repos.csv    # 组件仓库配置
│
└── docs/                # 文档
    └── repo.md          # 详细管理指南
```

## 组件分类

| 分类 | 说明 | 示例组件 |
|------|------|----------|
| **OS** | 操作系统项目 | arceos, axvisor, StarryOS |
| **Hypervisor** | 虚拟化相关组件 | arm_vcpu, arm_vgic, axvm, axvcpu |
| **ArceOS** | ArceOS 框架组件 | axcpu, axsched, axerrno, axio |
| **Starry** | StarryOS 相关组件 | starry-process, starry-signal, starry-vm |
| **rCore** | rCore 相关组件 | bitmap-allocator |

完整组件列表请查看 [scripts/repo/repos.csv](scripts/repo/repos.csv)。

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

# 拉取指定组件
python3 scripts/repo/repo.py pull <repo_name>

# 推送指定组件到远程
python3 scripts/repo/repo.py push <repo_name>

# 添加新组件
python3 scripts/repo/repo.py add --url <repo_url> --target <target_dir> --branch <branch>

# 切换组件分支
python3 scripts/repo/repo.py branch <repo_name> <branch>

# 从 CSV 初始化所有 subtrees
python3 scripts/repo/repo.py init -f scripts/repo/repos.csv
```

### 命令详解

#### `list` - 列出所有仓库

```bash
# 列出所有仓库
python3 scripts/repo/repo.py list

# 按分类过滤
python3 scripts/repo/repo.py list --category Hypervisor
python3 scripts/repo/repo.py list --category ArceOS
```

#### `pull` - 拉取更新

```bash
# 拉取指定组件
python3 scripts/repo/repo.py pull arm_vcpu

# 拉取指定组件的指定分支
python3 scripts/repo/repo.py pull arm_vcpu -b dev

# 拉取所有组件
python3 scripts/repo/repo.py pull --all

# 强制拉取（优先使用远程更改）
python3 scripts/repo/repo.py pull arm_vcpu --force
```

#### `push` - 推送更改

```bash
# 推送指定组件
python3 scripts/repo/repo.py push arm_vcpu

# 推送到指定分支
python3 scripts/repo/repo.py push arm_vcpu -b dev

# 推送所有组件
python3 scripts/repo/repo.py push --all
```

#### `add` - 添加新组件

```bash
python3 scripts/repo/repo.py add \
  --url https://github.com/org/repo \
  --target components/repo \
  --branch main \
  --category Hypervisor \
  --description "组件描述"
```

#### `remove` - 移除组件

```bash
# 从 CSV 中移除
python3 scripts/repo/repo.py remove <repo_name>

# 同时删除目录
python3 scripts/repo/repo.py remove <repo_name> --remove-dir
```

#### `branch` - 切换分支

```bash
python3 scripts/repo/repo.py branch <repo_name> <new_branch>
```

#### `init` - 初始化所有 subtrees

```bash
# 从 CSV 文件初始化所有 subtrees
python3 scripts/repo/repo.py init -f scripts/repo/repos.csv
```

## 工作流程

### 从组件仓库同步到主仓库

```
组件仓库更新 → 主仓库拉取更新
              ↓
    python3 scripts/repo/repo.py pull <repo_name>
```

### 从主仓库同步到组件仓库

```
主仓库修改组件代码 → 推送到组件仓库
                    ↓
        python3 scripts/repo/repo.py push <repo_name>
```

## 配置文件

### repos.csv 格式

```csv
url,branch,target_dir,category,description
https://github.com/org/repo,main,components/repo,Category,Description
```

| 字段 | 说明 |
|------|------|
| `url` | 仓库 URL |
| `branch` | 分支名（留空自动检测） |
| `target_dir` | 本地目标目录 |
| `category` | 分类（如 Hypervisor, ArceOS） |
| `description` | 描述信息 |

## 更多文档

- [详细仓库管理指南](docs/repo.md) - 包含完整的 Git Subtree 介绍、架构设计、GitHub Actions 配置、故障排查等内容

## 许可证

各组件遵循其独立的许可证，详见各组件目录下的 LICENSE 文件。
