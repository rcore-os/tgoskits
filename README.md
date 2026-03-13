# TGOSKits

[![Build & Test](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml/badge.svg)](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml)

## 1. 简介

TGOSKits 是一个面向操作系统开发的工具包集成仓库，通过 Git Subtree 技术将多个独立的组件仓库整合到一个统一的主仓库中，为操作系统开发提供完整的组件生态。

### 功能特性

TGOSKits 提供以下核心功能：

- **🎯 统一管理** - 在单一仓库中集中管理所有操作系统相关组件，简化依赖管理
- **📜 历史保留** - 完整保留每个组件的独立开发历史和提交记录，便于追溯和审计
- **🔄 双向同步** - 支持主仓库和组件仓库之间的双向代码同步，实现协作开发
- **🚀 独立开发** - 组件可以在独立仓库中开发、测试和发布，保持模块化
- **📦 版本控制** - 支持按分支或标签锁定组件版本，确保稳定性
- **🔧 灵活配置** - 通过 CSV 文件灵活配置组件，支持自动检测和手动指定

### 项目作用

TGOSKits 作为操作系统开发的工具包集成平台，整合了：

- **操作系统项目**：ArceOS、Axvisor、StarryOS
- **虚拟化组件**：支持 ARM/RISC-V/x86 架构的虚拟化相关库
- **系统框架**：ArceOS 框架的核心组件和驱动
- **宏内核系统**：StarryOS 宏内核系统的相关组件

通过统一的仓库管理，以便开发者可以：

1. 一键获取所有操作系统开发所需的组件
2. 在主仓库中统一修改和测试组件
3. 将修改同步回各个组件的独立仓库
4. 使用 CI/CD 实现自动化的双向同步

## 2. 组件管理

### 组件分类

TGOSKits 通过 `git subtree` 当前管理着 **60+ 组件**独立于各个子仓库中的组件，按照功能和应用场景分为以下几大类：

| 分类 | 数量 | 说明 | 代表组件 |
|------|------|------|----------|
| **Hypervisor** | 20 | 虚拟化相关组件 | `arm_vcpu`, `axvm`, `axvisor_api`, `riscv_vcpu` |
| **ArceOS** | 24 | ArceOS 框架核心组件 | `axcpu`, `axsched`, `axerrno`, `axio` |
| **OS** | 3 | 完整的操作系统项目 | `arceos`, `axvisor`, `StarryOS` |
| **Starry** | 9 | StarryOS 相关组件 | `starry-process`, `starry-signal`, `starry-vm` |
| **rCore** | 1 | rCore 生态组件 | `bitmap-allocator` |

### 管理工具

TGOSKits 提供了 `repo.py` 工具来管理组件：

```bash
# 列出所有组件
python3 scripts/repo/repo.py list

# 添加新组件
python3 scripts/repo/repo.py add --url <repo_url> --target <target_dir>

# 移除组件
python3 scripts/repo/repo.py remove <repo_name> --remove-dir

# 切换组件分支
python3 scripts/repo/repo.py branch <repo_name> <branch>

# 批量初始化
python3 scripts/repo/repo.py init -f scripts/repo/repos.csv
```

完整组件列表请查看 [scripts/repo/repos.csv](scripts/repo/repos.csv)。

## 3. 同步机制

TGOSKits 采用 **Git Subtree** 技术实现主仓库与组件仓库的双向同步。

### 手动同步

```bash
# 从组件仓库拉取更新到主仓库
python3 scripts/repo/repo.py pull <repo_name>

# 从主仓库推送更改到组件仓库
python3 scripts/repo/repo.py push <repo_name>

# 拉取/推送所有组件
python3 scripts/repo/repo.py pull --all
python3 scripts/repo/repo.py push --all
```

### 自动同步

TGOSKits 配置了两个 GitHub Actions 工作流实现自动化同步：

- **Pull 工作流** (`.github/workflows/pull.yml`)：从子仓库拉取更新到主仓库
  - 支持手动触发和 `repository_dispatch` 事件触发
  - 自动将更新合并到指定分支（默认 `next`）

- **Push 工作流** (`.github/workflows/push.yml`)：从主仓库推送更改到子仓库
  - 自动检测修改的组件
  - 推送到子仓库的 `mirror` 分支

详细的同步机制和配置请查看 [docs/repo.md](docs/repo.md)。

## 4. 快速开始

### 环境要求

- **Rust**: 1.75+ (edition 2024)
- **Python**: 3.6+
- **Git**: 2.0+

### 克隆仓库

```bash
# 克隆主仓库（包含所有组件）
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits
```

### 开发流程

#### 修改组件代码

1. 在主仓库中修改组件代码：
   ```bash
   vim components/arm_vcpu/src/lib.rs
   ```

2. 提交更改：
   ```bash
   git add components/arm_vcpu/
   git commit -m "feat(arm_vcpu): add new feature"
   ```
   
3. 将更改推送到当前主仓库：
   ```bash
   git push origin main
   ```
   
3. 【可选】推送到独立组件子仓库：
   ```bash
   python3 scripts/repo/repo.py push arm_vcpu
   ```

#### 同步组件更新

1. 拉取组件仓库的更新：
   ```bash
   python3 scripts/repo/repo.py pull arm_vcpu
   ```

2. 推送到主仓库：
   ```bash
   git push origin main
   ```

### 构建和测试

```bash
# 构建所有组件
cargo build

# 运行测试
cargo test

# 构建特定组件
cargo build -p arm_vcpu

# 运行特定组件的测试
cargo test -p arm_vcpu
```

## 5. 贡献

我们欢迎所有形式的贡献！

### 如何贡献

1. **Fork 仓库**：点击 GitHub 页面右上角的 Fork 按钮

2. **创建分支**：
   ```bash
   git checkout -b feature/your-feature-name
   ```

3. **进行修改**：
   - 遵循 Rust 代码规范
   - 添加必要的测试
   - 更新相关文档

4. **提交更改**：
   ```bash
   git commit -m "feat: add your feature"
   ```

5. **推送分支**：
   ```bash
   git push origin feature/your-feature-name
   ```

6. **创建 Pull Request**：
   - 在 GitHub 上创建 Pull Request
   - 描述你的修改和动机
   - 等待代码审查

### 贡献指南

- **代码规范**：遵循 Rust 标准代码规范
- **提交信息**：使用清晰的提交信息（参考 [Conventional Commits](https://www.conventionalcommits.org/)）
- **测试**：确保所有测试通过
- **文档**：更新相关文档和注释

### 报告问题

如果您发现了 bug 或有功能建议，请：

1. 检查是否已有相关问题
2. 创建新的 Issue，详细描述问题或建议
3. 提供复现步骤（如果是 bug）

## 6. 许可证

TGOSKits 采用 `Apache-2.0` 许可协议，各组件可能有其独立的许可证，详见各组件目录下的 LICENSE 文件。
